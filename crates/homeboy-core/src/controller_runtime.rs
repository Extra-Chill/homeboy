//! Immutable controller executable provenance for durable orchestration work.

use fs4::fs_std::FileExt;
use homeboy_engine_primitives::content_hash;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, SystemTime};
use uuid::Uuid;

use crate::{build_identity, paths, Error, Result};

pub const CONTROLLER_RUNTIME_METADATA_KEY: &str = "controller_runtime";
#[cfg(any(test, feature = "test-support"))]
pub(crate) const TEST_CONTROLLER_RUNTIME_EXECUTABLE_ENV: &str =
    "HOMEBOY_TEST_CONTROLLER_RUNTIME_EXECUTABLE";
/// Names the executable [`TEST_CONTROLLER_RUNTIME_EXECUTABLE_ENV`]'s fixture is
/// copied from.
///
/// The fixture is materialized on first *read* rather than when the hermetic
/// home is built, so the destination path can name a file that does not exist
/// yet. This variable is what lets any process holding the destination —
/// including a child test process that inherited it — produce those bytes
/// itself. See `test_support::ensure_test_controller_fixture`.
#[cfg(any(test, feature = "test-support"))]
pub(crate) const TEST_CONTROLLER_RUNTIME_SOURCE_ENV: &str =
    "HOMEBOY_TEST_CONTROLLER_RUNTIME_SOURCE";
#[cfg(any(test, feature = "test-support"))]
pub(crate) const TEST_CONTROLLER_RUNTIME_STORE_ENV: &str = "HOMEBOY_TEST_CONTROLLER_RUNTIME_STORE";
#[cfg(any(test, feature = "test-support"))]
pub(crate) const TEST_CONTROLLER_RUNTIME_IDENTITY_ENV: &str =
    "HOMEBOY_TEST_CONTROLLER_RUNTIME_IDENTITY";

const ACTIVE_GENERATION_FILE: &str = "active.json";
const ADMISSION_LOCK_DIR: &str = "admission.lock";
const ADMISSION_QUEUE_FILE: &str = "admission-queue.json";
const ADMISSION_QUEUE_LOCK_FILE: &str = "admission-queue.lock";
const ADMISSION_OWNER_SCHEMA: &str = "homeboy/controller-admission-owner/v1";
const ADMISSION_QUEUE_SCHEMA: &str = "homeboy/controller-admission-queue/v1";
/// Resolved admission timings for one wait.
///
/// Loaded once per operation and then carried, rather than re-read per poll:
/// `load_config` clones the entire product config, which is not something the
/// inner loop of the global admission lock should do on every iteration.
///
/// These were previously hardcoded constants whose only override was gated
/// behind `cfg(any(test, feature = "test-support"))`, so a release binary could
/// not tune them at all. They now come from
/// [`crate::defaults::ControllerAdmissionConfig`], with identical defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AdmissionTimings {
    poll: Duration,
    poll_max: Duration,
    lease: Duration,
    heartbeat: Duration,
    wait_timeout: Duration,
    busy_wait: Duration,
}

impl AdmissionTimings {
    fn load() -> Self {
        Self::sanitized(&crate::defaults::load_config().controller_admission)
    }

    /// Project config onto timings that cannot strand the queue.
    ///
    /// Config is sanitized rather than rejected because every detached cook
    /// passes through this lock: an implausible value must degrade to a safe
    /// timing, not fail the whole orchestrator closed.
    ///
    /// The clamp chain establishes `floor <= poll <= poll_max <= heartbeat <=
    /// lease / 2`. Each link is load-bearing:
    ///
    /// * A zero interval would spin on the durable queue file rather than wait.
    /// * A waiter renews its lease on the heartbeat and is reclaimed as crashed
    ///   once the lease expires, so `heartbeat >= lease` would evict *live*
    ///   waiters from their own queue slots — silently destroying FIFO order on
    ///   a lock whose entire purpose is FIFO order. Renewing at least twice per
    ///   lease keeps one slow queue write from expiring a live waiter.
    /// * The loop can only heartbeat when it wakes, so a sleep longer than the
    ///   heartbeat would let a waiter sleep through its own renewal deadline.
    ///   Bounding `poll_max` by the heartbeat is what makes backoff unable to
    ///   cost a waiter the queue slot it is backing off to keep.
    fn sanitized(config: &crate::defaults::ControllerAdmissionConfig) -> Self {
        let floor = crate::defaults::MIN_ADMISSION_INTERVAL_MS;
        let lease = config.queue_lease_ms.max(floor);
        let heartbeat = config
            .queue_heartbeat_ms
            .clamp(floor, (lease / 2).max(floor));
        let poll = config.queue_poll_ms.clamp(floor, heartbeat);
        let poll_max = config.queue_poll_max_ms.clamp(poll, heartbeat);
        Self {
            poll: Duration::from_millis(poll),
            poll_max: Duration::from_millis(poll_max),
            lease: Duration::from_millis(lease),
            heartbeat: Duration::from_millis(heartbeat),
            wait_timeout: Duration::from_millis(config.queue_wait_timeout_ms),
            busy_wait: Duration::from_millis(config.busy_wait_ms),
        }
    }
}

/// Next sleep for a waiter whose queue position did not move.
///
/// FIFO position is what grants admission, so a waiter that is not at the head
/// gains nothing by re-reading the durable queue every 250ms — at the default
/// timeout that is 2,400 reads and 2,400 wakeups per waiter, multiplied across
/// the whole submission wave. The interval doubles up to the configured
/// ceiling, and the caller resets it to `poll` the moment the queue moves, so a
/// waiter converges back to a tight poll exactly as its turn approaches.
fn next_admission_backoff(current: Duration, timings: &AdmissionTimings) -> Duration {
    current.saturating_mul(2).min(timings.poll_max)
}

/// Render a wait in the unit an operator reasons in. "600000ms" tells nobody
/// anything; "10m0s" does.
fn format_admission_duration(duration: Duration) -> String {
    let seconds = duration.as_secs();
    if seconds == 0 {
        return format!("{}ms", duration.as_millis());
    }
    if seconds < 60 {
        return format!("{seconds}s");
    }
    format!("{}m{}s", seconds / 60, seconds % 60)
}

fn admission_queue_lease() -> Duration {
    AdmissionTimings::load().lease
}

fn admission_queue_wait_timeout() -> Duration {
    AdmissionTimings::load().wait_timeout
}

fn admission_busy_wait() -> Duration {
    AdmissionTimings::load().busy_wait
}
static ADMISSION_LOCK_PROCESS_GUARDS: OnceLock<Mutex<BTreeMap<PathBuf, &'static Mutex<()>>>> =
    OnceLock::new();
#[cfg(test)]
static TEST_ADMISSION_HEAD_BARRIER: OnceLock<Mutex<Option<std::sync::Arc<std::sync::Barrier>>>> =
    OnceLock::new();
#[cfg(test)]
static TEST_ADMISSION_OWNER_CAS_REPLACEMENT: OnceLock<Mutex<Option<Value>>> = OnceLock::new();
#[cfg(all(unix, any(test, feature = "test-support")))]
static TEST_CONTROLLER_FIXTURE_DIGESTS: OnceLock<Mutex<BTreeMap<ExecutableFileIdentity, String>>> =
    OnceLock::new();
/// Digests this process has already computed, keyed by observed file identity.
///
/// Sealing a cook into an immutable runtime hashes the same bytes repeatedly:
/// `pin_executable` hashes its source, `publish_pin` hashes the staged copy or
/// an existing destination, `validate_pin` hashes the destination, and
/// `admit_current_for` then validates the resulting pin a second time -- before
/// the re-exec'd child repeats the whole sequence against the pin it is already
/// executing. Each of those is a full SHA-256 of the controller binary, which
/// for an unoptimized build is a multi-hundred-megabyte read costing tens of
/// seconds.
///
/// Memoizing is sound because the key is the file's observed identity, not its
/// path alone: writing to a file moves its modification and change times, and
/// replacing it moves its inode. A hit therefore means nothing has touched
/// those bytes since this process hashed them. The memo is process local and
/// never persisted, so it cannot outlive the observation it was derived from.
/// How long a hash must take before its result may be memoized.
///
/// The memo is only sound while the observed identity distinguishes the bytes
/// that were hashed. Inode and size do that for a replacement or a resize;
/// modification and change time are what carry an in-place, same-size rewrite.
/// Those come from the kernel's coarse timestamp clock, which advances every
/// 1ms on this ext4 host -- so a rewrite landing in the same tick as the hash
/// moves no observed field at all, and the memo would answer for bytes that no
/// longer exist.
///
/// Hash duration closes that window without needing to know the granularity:
/// if hashing outlasts a tick, any write racing it necessarily lands in a later
/// tick and moves `mtime`. If hashing is faster than a tick the observation is
/// not durable and is not memoized -- which costs nothing, because a file that
/// hashes that fast is cheap to hash again. The controller binaries this memo
/// exists for are hundreds of megabytes and take seconds.
#[cfg(unix)]
const DIGEST_MEMO_MIN_HASH_TIME: std::time::Duration = std::time::Duration::from_millis(10);

/// Test override for [`DIGEST_MEMO_MIN_HASH_TIME`], in milliseconds.
///
/// The guard is defined in terms of a duration no unit-test-sized file can
/// reach, so memoization itself is unreachable in a test without a seam. Tests
/// that are about the memo lower this; the test that is about the guard leaves
/// it alone.
#[cfg(all(test, unix))]
static DIGEST_MEMO_MIN_HASH_TIME_MS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(u64::MAX);

#[cfg(unix)]
fn digest_memo_min_hash_time() -> std::time::Duration {
    #[cfg(test)]
    {
        let configured = DIGEST_MEMO_MIN_HASH_TIME_MS.load(std::sync::atomic::Ordering::Relaxed);
        if configured != u64::MAX {
            return std::time::Duration::from_millis(configured);
        }
    }
    DIGEST_MEMO_MIN_HASH_TIME
}

#[cfg(unix)]
static EXECUTABLE_DIGESTS: OnceLock<Mutex<BTreeMap<ExecutableFileIdentity, String>>> =
    OnceLock::new();
#[cfg(all(test, unix))]
static EXECUTABLE_DIGEST_COMPUTATIONS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);
#[cfg(all(test, unix))]
static TEST_CONTROLLER_FIXTURE_DIGEST_CALLS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// One executable file as this process observed it. Inode and change time are
/// part of the identity so replacing or modifying a path cannot reuse its prior
/// digest.
///
/// This is the single definition of "same file" for both the process-local
/// digest memo and the test fixture digest cache. It used to be duplicated as a
/// verbatim `TestExecutableFileIdentity` clone -- same eight fields, same order,
/// same derives -- which meant strengthening the production identity left the
/// fixture cache keyed on the older, weaker one and the tests silently stopped
/// exercising what production does.
#[cfg(unix)]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ExecutableFileIdentity {
    path: PathBuf,
    device: u64,
    inode: u64,
    size: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

/// Report-only retention inventory for immutable controller runtime pins.
///
/// No current cleanup command deletes controller runtime pins. This report is
/// intentionally the eligibility primitive for a future narrowly-scoped pruner;
/// callers must retain every path in `retained` and may consider only `eligible`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControllerRuntimeRetentionReport {
    pub retained: Vec<PathBuf>,
    pub eligible: Vec<PathBuf>,
    pub snapshots: Vec<ControllerRuntimeSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ControllerRuntimeSnapshot {
    pub identity: String,
    pub path: PathBuf,
    pub size_bytes: u64,
    pub age_seconds: u64,
    pub pins: Vec<PathBuf>,
    pub retention_reasons: Vec<String>,
    pub eligible: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ControllerRuntimeCleanupOptions {
    pub apply: bool,
    pub min_age: Duration,
    pub max_total_bytes: u64,
    pub limit: usize,
}

/// Operator overrides layered on top of the configured controller runtime
/// retention window.
///
/// Controller runtime cleanup has two supported entry points — `homeboy
/// cleanup --include controller-runtimes` and `homeboy runtime
/// controller-prune`. Both resolve their effective policy through
/// [`resolve_cleanup_options`] so one command cannot honor the operator's
/// configured window while the other silently deletes outside it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ControllerRuntimeRetentionOverrides {
    /// Operator `--limit`. `None` uses the configured retention limit.
    pub limit: Option<i64>,
    /// Explicit opt-in to a policy-free purge. Deleting pins the configured
    /// window still protects is destructive, so it is never the default.
    pub ignore_retention: bool,
}

/// Resolve the effective cleanup policy from persisted configuration.
///
/// This is the single place controller runtime retention becomes concrete
/// numbers. Call sites pass only what an operator typed; they never invent a
/// window of their own.
pub fn resolve_cleanup_options(
    apply: bool,
    overrides: ControllerRuntimeRetentionOverrides,
) -> ControllerRuntimeCleanupOptions {
    cleanup_options_from_retention(apply, overrides, &crate::defaults::load_config().retention)
}

fn cleanup_options_from_retention(
    apply: bool,
    overrides: ControllerRuntimeRetentionOverrides,
    retention: &crate::defaults::RetentionConfig,
) -> ControllerRuntimeCleanupOptions {
    if overrides.ignore_retention {
        // The historical unbounded purge, now reachable only by explicit
        // operator opt-in: every eligible identity is expired and over budget.
        return ControllerRuntimeCleanupOptions {
            apply,
            min_age: Duration::ZERO,
            max_total_bytes: 0,
            limit: usize::MAX,
        };
    }
    ControllerRuntimeCleanupOptions {
        apply,
        min_age: Duration::from_secs(retention.controller_runtime_days.saturating_mul(86_400)),
        max_total_bytes: retention.controller_runtime_max_bytes,
        // A nonsensical negative limit fails closed at zero removals rather
        // than widening into an unbounded delete.
        limit: usize::try_from(overrides.limit.unwrap_or(retention.limit)).unwrap_or(0),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ControllerRuntimePruneResult {
    pub retained: Vec<PathBuf>,
    pub eligible: Vec<PathBuf>,
    pub removed: Vec<PathBuf>,
    pub removed_identities: Vec<PathBuf>,
    pub reclaimed_bytes: u64,
    pub snapshots: Vec<ControllerRuntimeSnapshot>,
}

/// Discover pin references through the durable lifecycle store and classify the
/// content-addressed pins currently present on disk. Queued, running, and
/// recoverable partial records retain their pins because lifecycle recovery can
/// still operate on them. The active admission generation is retained as well.
pub fn retention_report() -> Result<ControllerRuntimeRetentionReport> {
    let referenced = crate::controller_pin_reference::referenced_controller_pins()?;
    retention_report_with_references_at(&runtime_root()?, &referenced, SystemTime::now())
}

/// Return executables selected by a durable recovery record or the current
/// controller generation. These paths can live in a source checkout as well as
/// the content-addressed runtime store, so repository artifact cleanup uses this
/// projection to avoid removing a selected recovery executable.
pub fn protected_executables() -> Result<Vec<PathBuf>> {
    let mut protected: BTreeSet<_> = crate::controller_pin_reference::referenced_controller_pins()?
        .into_iter()
        .collect();
    let active = runtime_root()?.join(ACTIVE_GENERATION_FILE);
    if active.exists() {
        let value = fs::read_to_string(&active).map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("read active controller generation".to_string()),
            )
        })?;
        let runtime: Value = serde_json::from_str(&value).map_err(|error| {
            Error::validation_invalid_json(
                error,
                Some("parse active controller generation".to_string()),
                None,
            )
        })?;
        for pointer in ["/originating/executable", "/originating/pinned_executable"] {
            if let Some(path) = runtime.pointer(pointer).and_then(Value::as_str) {
                protected.insert(PathBuf::from(path));
            }
        }
    }
    Ok(protected.into_iter().collect())
}

fn retention_report_with_references_at(
    root: &Path,
    referenced: &[PathBuf],
    now: SystemTime,
) -> Result<ControllerRuntimeRetentionReport> {
    let mut retained = BTreeSet::new();

    for path in referenced {
        if content_addressed_pin_path(&root, path) {
            retained.insert(path.clone());
        }
    }

    let active = root.join(ACTIVE_GENERATION_FILE);
    if active.exists() {
        let value = fs::read_to_string(&active).map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("read active controller generation".to_string()),
            )
        })?;
        let runtime: Value = serde_json::from_str(&value).map_err(|error| {
            Error::validation_invalid_json(
                error,
                Some("parse active controller generation".to_string()),
                None,
            )
        })?;
        if let Some(path) = runtime
            .pointer("/originating/pinned_executable")
            .and_then(Value::as_str)
            .map(PathBuf::from)
            .filter(|path| content_addressed_pin_path(&root, path))
        {
            retained.insert(path);
        }
    }

    let pins = discover_pin_paths(&root)?;
    let eligible = pins.difference(&retained).cloned().collect();
    let mut snapshots = Vec::new();
    for entry in fs::read_dir(&root).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("list controller runtime identities".to_string()),
        )
    })? {
        let entry = entry.map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("read controller runtime identity".to_string()),
            )
        })?;
        let path = entry.path();
        if !path.is_dir()
            || path
                .file_name()
                .is_some_and(|name| name == ADMISSION_LOCK_DIR)
        {
            continue;
        }
        let identity_pins = pins
            .iter()
            .filter(|pin| pin.starts_with(&path))
            .cloned()
            .collect::<Vec<_>>();
        if identity_pins.is_empty() {
            continue;
        }
        let mut reasons = Vec::new();
        if identity_pins.iter().any(|pin| retained.contains(pin)) {
            reasons.push("pinned_by_active_or_resumable_run_or_current_generation".to_string());
        }
        let modified = fs::metadata(&path)
            .and_then(|metadata| metadata.modified())
            .unwrap_or(now);
        let age_seconds = now.duration_since(modified).unwrap_or_default().as_secs();
        snapshots.push(ControllerRuntimeSnapshot {
            identity: path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string(),
            size_bytes: path_size(&path),
            age_seconds,
            pins: identity_pins,
            eligible: reasons.is_empty(),
            retention_reasons: reasons,
            path,
        });
    }
    snapshots.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(ControllerRuntimeRetentionReport {
        retained: retained.into_iter().collect(),
        eligible,
        snapshots,
    })
}

/// Remove only content-addressed pins not referenced by a nonterminal durable
/// record or the active generation, and only those the configured retention
/// window no longer protects. The caller chooses mutation explicitly, and may
/// opt out of the window explicitly through the overrides.
pub fn prune_pins(
    apply: bool,
    overrides: ControllerRuntimeRetentionOverrides,
) -> Result<ControllerRuntimePruneResult> {
    cleanup(resolve_cleanup_options(apply, overrides))
}

/// Inventory and reclaim immutable runtime identities. The admission lock makes
/// the final reachability check atomic with activation and materialization.
pub fn cleanup(options: ControllerRuntimeCleanupOptions) -> Result<ControllerRuntimePruneResult> {
    cleanup_in_root(&runtime_root()?, options)
}

/// [`cleanup`] under an already-resolved runtime root.
pub fn cleanup_in_root(
    root: &Path,
    options: ControllerRuntimeCleanupOptions,
) -> Result<ControllerRuntimePruneResult> {
    // Lifecycle inventory may migrate legacy records, which itself needs the
    // admission lock. Collect reachability before taking the runtime lock.
    let referenced = crate::controller_pin_reference::referenced_controller_pins()?;
    let _lock = acquire_admission_lock(&root.join(ADMISSION_LOCK_DIR))?;
    if options.apply {
        recover_cleanup_tombstones(&root)?;
    }
    let mut report = retention_report_with_references_at(root, &referenced, SystemTime::now())?;
    let mut total = report
        .snapshots
        .iter()
        .map(|snapshot| snapshot.size_bytes)
        .sum::<u64>();
    let mut candidates = report
        .snapshots
        .iter_mut()
        .filter(|snapshot| snapshot.eligible)
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        right
            .age_seconds
            .cmp(&left.age_seconds)
            .then_with(|| right.size_bytes.cmp(&left.size_bytes))
    });
    let mut removed = Vec::new();
    let mut removed_identities = Vec::new();
    let mut reclaimed_bytes: u64 = 0;
    for snapshot in candidates {
        let expired = snapshot.age_seconds >= options.min_age.as_secs();
        let pressured = total > options.max_total_bytes;
        if !(expired || pressured) {
            snapshot
                .retention_reasons
                .push("within_age_and_size_budget".to_string());
            continue;
        }
        if removed.len() >= options.limit {
            snapshot
                .retention_reasons
                .push("cleanup_limit_reached".to_string());
            continue;
        }
        if options.apply {
            // Rename first: an interrupted cleanup leaves a non-discoverable
            // tombstone rather than a partially materialized identity.
            let tombstone = root.join(format!(".cleanup-{}", Uuid::new_v4()));
            fs::rename(&snapshot.path, &tombstone).map_err(|error| {
                Error::internal_io(
                    error.to_string(),
                    Some("stage controller runtime identity cleanup".to_string()),
                )
            })?;
            fs::remove_dir_all(&tombstone).map_err(|error| {
                Error::internal_io(
                    error.to_string(),
                    Some("remove controller runtime identity".to_string()),
                )
            })?;
            removed.extend(snapshot.pins.clone());
            removed_identities.push(snapshot.path.clone());
            reclaimed_bytes = reclaimed_bytes.saturating_add(snapshot.size_bytes);
            total = total.saturating_sub(snapshot.size_bytes);
        }
    }
    let removed_set = removed.iter().collect::<BTreeSet<_>>();
    report.eligible.retain(|path| !removed_set.contains(path));
    Ok(ControllerRuntimePruneResult {
        retained: report.retained,
        eligible: report.eligible,
        removed,
        removed_identities,
        reclaimed_bytes,
        snapshots: report.snapshots,
    })
}

fn recover_cleanup_tombstones(root: &Path) -> Result<()> {
    for entry in fs::read_dir(root).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("list interrupted controller runtime cleanup".to_string()),
        )
    })? {
        let path = entry
            .map_err(|error| {
                Error::internal_io(
                    error.to_string(),
                    Some("read interrupted controller runtime cleanup".to_string()),
                )
            })?
            .path();
        if path.is_dir()
            && path
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with(".cleanup-"))
        {
            fs::remove_dir_all(path).map_err(|error| {
                Error::internal_io(
                    error.to_string(),
                    Some("complete interrupted controller runtime cleanup".to_string()),
                )
            })?;
        }
    }
    Ok(())
}

fn path_size(path: &Path) -> u64 {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return 0;
    };
    if metadata.is_file() {
        return metadata.len();
    }
    fs::read_dir(path)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok())
        .map(|entry| path_size(&entry.path()))
        .sum()
}

fn content_addressed_pin_path(root: &Path, path: &Path) -> bool {
    let Ok(relative) = path.strip_prefix(root) else {
        return false;
    };
    let components = relative.components().collect::<Vec<_>>();
    let primary_pin = matches!(
        components.as_slice(),
        [generation, executable]
            if generation.as_os_str() != "active.json" && executable.as_os_str() == "homeboy"
    );
    let recovered_pin = matches!(
        components.as_slice(),
        [generation, recovery, executable]
            if generation.as_os_str() != "active.json"
                && recovery.as_os_str().to_string_lossy().starts_with("recovery-")
                && executable.as_os_str() == "homeboy"
    );
    primary_pin || recovered_pin
}

fn discover_pin_paths(root: &Path) -> Result<BTreeSet<PathBuf>> {
    let mut pins = BTreeSet::new();
    for entry in fs::read_dir(root).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("list controller runtime pins".to_string()),
        )
    })? {
        let entry = entry.map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("read controller runtime pin".to_string()),
            )
        })?;
        let path = entry.path();
        if path
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with(".cleanup-"))
        {
            continue;
        }
        let direct_pin = path.join("homeboy");
        if content_addressed_pin_path(root, &direct_pin) && direct_pin.is_file() {
            pins.insert(direct_pin);
        }
        if !path.is_dir() {
            continue;
        }
        for recovery in fs::read_dir(&path).map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("list recovered controller runtime pins".to_string()),
            )
        })? {
            let recovery = recovery.map_err(|error| {
                Error::internal_io(
                    error.to_string(),
                    Some("read recovered controller runtime pin".to_string()),
                )
            })?;
            let recovered_pin = recovery.path().join("homeboy");
            if content_addressed_pin_path(root, &recovered_pin) && recovered_pin.is_file() {
                pins.insert(recovered_pin);
            }
        }
    }
    Ok(pins)
}

/// Holds the short admission critical section.  Keeping selection and durable
/// record creation together prevents a submission from observing A after B is
/// published.
pub struct RuntimeAdmission {
    _lock: AdmissionLock,
    pub runtime: Value,
}

#[derive(Debug)]
struct AdmissionLock {
    path: PathBuf,
    token: String,
    request_id: String,
    _process_guard: MutexGuard<'static, ()>,
    file: fs::File,
}

impl Drop for AdmissionLock {
    fn drop(&mut self) {
        // The advisory lock serializes record updates, so a guard only clears
        // the owner record that it published. The file remains as the durable
        // coordination inode; deleting it would permit a second inode/lock.
        if admission_owner_token(&self.path).as_deref() == Some(self.token.as_str()) {
            let _ = fs::write(&self.path, b"");
        }
        let _ = update_admission_queue(&self.path, |queue| {
            if queue["owner"]["request_id"].as_str() == Some(self.request_id.as_str()) {
                queue["owner"] = Value::Null;
                queue["requests"] = queue["requests"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|request| {
                        request["request_id"].as_str() != Some(self.request_id.as_str())
                    })
                    .collect();
            }
        });
        let _ = self.file.unlock();
    }
}

/// `pin_current` under an already-resolved runtime root.
pub fn pin_current_in_root(root: &Path) -> Result<Value> {
    let _lock = acquire_admission_lock(&root.join(ADMISSION_LOCK_DIR))?;
    pin_current_unlocked()
}

fn pin_current_unlocked() -> Result<Value> {
    let identity = build_identity::current();
    let executable = current_executable()?;
    pin_executable_with_source(
        &executable,
        &identity.display,
        build_source_provenance(&identity),
    )
}

/// Pin the currently executing controller while participating in the FIFO
/// admission queue.  Use this instead of `pin_current()` when concurrent cook
/// requests must wait their turn rather than fast-fail.
#[cfg(test)]
pub fn pin_current_queued(
    request_id: &str,
    cancellation_requested: impl Fn() -> Result<bool>,
) -> Result<Value> {
    pin_current_queued_in_root(&runtime_root()?, request_id, cancellation_requested)
}

/// [`pin_current_queued`] under an already-resolved runtime root.
pub fn pin_current_queued_in_root(
    root: &Path,
    request_id: &str,
    cancellation_requested: impl Fn() -> Result<bool>,
) -> Result<Value> {
    let lock_path = root.join(ADMISSION_LOCK_DIR);
    let request_id = request_id.trim();
    if request_id.is_empty() {
        return Err(Error::validation_invalid_argument(
            "controller_admission",
            "controller admission request ID is required",
            None,
            None,
        ));
    }
    enqueue_admission_request(&lock_path, request_id)?;
    let lock = match acquire_queued_admission_lock(&lock_path, request_id, &cancellation_requested)
    {
        Ok(lock) => lock,
        Err(error) => {
            let _ = remove_admission_request(&lock_path, request_id);
            return Err(error);
        }
    };
    let runtime = pin_current_unlocked()?;
    drop(lock);
    Ok(runtime)
}

fn current_executable() -> Result<PathBuf> {
    #[cfg(any(test, feature = "test-support"))]
    if let Some(executable) = std::env::var_os(TEST_CONTROLLER_RUNTIME_EXECUTABLE_ENV) {
        let executable = PathBuf::from(executable);
        // The fixture is materialized here, on first read, instead of when the
        // hermetic home was built: only the handful of readers of this contract
        // need its bytes, and the copy is of a multi-hundred-megabyte binary.
        crate::test_support::ensure_test_controller_fixture(&executable);
        return Ok(executable);
    }

    std::env::current_exe().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("resolve controller executable".to_string()),
        )
    })
}

/// Seal a verified executable for a command-scoped controller continuation.
/// The returned pin is content-addressed, executable, and self-identifying.
pub fn pin_executable(executable: &Path, identity: &str) -> Result<Value> {
    pin_executable_with_source(executable, identity, unavailable_source_provenance())
}

fn pin_executable_with_source(executable: &Path, identity: &str, source: Value) -> Result<Value> {
    let digest = controller_executable_digest(executable)?;
    let pinned_path = pinned_path(identity, &digest)?;
    publish_pin(executable, &pinned_path, &digest)?;

    let runtime = runtime_pin(identity, executable, &pinned_path, &digest, source);
    validate_pin(&runtime)?;
    Ok(runtime)
}

/// Build and seal a controller executable from the exact source revision the
/// runner already verified. This is intentionally explicit: runner refreshes
/// must not depend on controller network or build availability.
pub fn materialize_source_commit(source: &str, commit: &str, identity: &str) -> Result<Value> {
    let workspace = tempfile::tempdir().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("create controller candidate workspace".to_string()),
        )
    })?;
    let checkout = workspace.path().join("source");
    run_command(
        "git",
        ["clone", "--quiet", source, &checkout.display().to_string()],
    )?;
    run_command(
        "git",
        [
            "-C",
            &checkout.display().to_string(),
            "checkout",
            "--quiet",
            "--detach",
            commit,
        ],
    )?;
    let resolved = Command::new("git")
        .args(["-C", &checkout.display().to_string(), "rev-parse", "HEAD"])
        .output()
        .map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("verify controller candidate source commit".to_string()),
            )
        })?;
    if !resolved.status.success() || String::from_utf8_lossy(&resolved.stdout).trim() != commit {
        return Err(Error::validation_invalid_argument(
            "commit",
            "controller candidate source did not resolve the requested exact commit",
            Some(commit.to_string()),
            None,
        ));
    }
    let target =
        crate::cleanup::acquire_shared_cargo_target(&format!("controller-runtime:{commit}"))?;
    let build = Command::new("cargo")
        .args(["build", "--release", "--bin", "homeboy"])
        .env("CARGO_TARGET_DIR", target.target_dir())
        .current_dir(&checkout)
        .status()
        .map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("build controller candidate source".to_string()),
            )
        })?;
    if !build.success() {
        return Err(Error::validation_invalid_argument(
            "source",
            format!("controller candidate build failed with status {build}"),
            Some(source.to_string()),
            None,
        ));
    }
    pin_executable_with_source(
        &target.target_dir().join("release/homeboy"),
        identity,
        json!({
            "repository": source,
            "revision": commit,
            "verification": "explicit_source_commit",
        }),
    )
}

fn runtime_pin(
    identity: &str,
    executable: &Path,
    pinned_path: &Path,
    digest: &str,
    source: Value,
) -> Value {
    json!({
        "schema": "homeboy/controller-runtime-pin/v2",
        "requested": identity,
        "originating": {
            "build_identity": identity,
            "executable": executable,
            "pinned_executable": pinned_path,
            "sha256": digest,
            "source": source,
        },
        "current": identity,
        "executed": identity,
    })
}

/// Pin the process submitting a durable run while serializing admission. The
/// active-generation pointer is diagnostic state only: every fresh run must
/// retain the executable that created it, rather than inherit a previous
/// controller's selection.
/// Ambient admission entry points, retained for this module's own tests only.
///
/// These resolve the runtime root from process-global state. That is the exact
/// split `tests/nextest_shard_parallelism_test.rs` recorded: a test holding a
/// `HermeticTestContext` store while production code beneath it read the
/// admission lease out of the real `$HOME`, so one test observed another's
/// `controller_admission.owner`. `HermeticTestContext` does not mutate the
/// environment, so process-per-test isolation cannot help there.
///
/// Every production caller now supplies a root, so the `#[cfg(test)]` gate below
/// is the compiler enforcing that: production cannot reach ambient admission
/// even by accident, and a future caller that tries will fail to build rather
/// than fail a shard at random (#7505).
///
/// The module's own tests keep using them through `with_isolated_home`, which
/// mutates `HOME` and is therefore safe under process-per-test.
#[cfg(test)]
pub fn admit_current() -> Result<RuntimeAdmission> {
    admit_current_for(&format!("controller-{}", Uuid::new_v4()))
}

/// Admit a durable controller request in FIFO order. The request ID is normally
/// the agent-task run ID, which lets another controller observe or cancel a
/// waiting admission after the original process has exited.
#[cfg(test)]
pub fn admit_current_for(request_id: &str) -> Result<RuntimeAdmission> {
    admit_current_for_with_cancellation_check(request_id, || Ok(false))
}

/// Admit a request while checking the caller's durable lifecycle state at the
/// queue claim boundary. The check runs under the queue lock, after the
/// advisory lock is acquired and before ownership can be published.
#[cfg(test)]
pub fn admit_current_for_with_cancellation_check(
    request_id: &str,
    cancellation_requested: impl Fn() -> Result<bool>,
) -> Result<RuntimeAdmission> {
    admit_current_for_with_cancellation_check_in_root(
        &runtime_root()?,
        request_id,
        cancellation_requested,
    )
}

/// [`admit_current_for_with_cancellation_check`] under an already-resolved runtime root.
pub fn admit_current_for_with_cancellation_check_in_root(
    root: &Path,
    request_id: &str,
    cancellation_requested: impl Fn() -> Result<bool>,
) -> Result<RuntimeAdmission> {
    let lock_path = root.join(ADMISSION_LOCK_DIR);
    let request_id = request_id.trim();
    if request_id.is_empty() {
        return Err(Error::validation_invalid_argument(
            "controller_admission",
            "controller admission request ID is required",
            None,
            None,
        ));
    }
    enqueue_admission_request(&lock_path, request_id)?;
    let lock = match acquire_queued_admission_lock(&lock_path, request_id, &cancellation_requested)
    {
        Ok(lock) => lock,
        Err(error) => {
            // A failed foreground admission must not leave a live-looking head
            // behind. Cancellation may already have removed the request.
            let _ = remove_admission_request(&lock_path, request_id);
            return Err(error);
        }
    };
    let runtime = pin_current_unlocked()?;
    write_active_generation(&root.join(ACTIVE_GENERATION_FILE), &runtime)?;
    validate_pin(&runtime)?;
    heartbeat_admission_owner(&lock_path, request_id, Some(&runtime))?;
    Ok(RuntimeAdmission {
        _lock: lock,
        runtime,
    })
}

/// Return the durable admission view used by lifecycle status output.
#[cfg(test)]
pub fn admission_status(request_id: &str) -> Result<Value> {
    admission_status_at(&runtime_root()?, request_id)
}

/// Read the durable admission view from an explicitly selected
/// controller-runtime store. Lifecycle stores use this to keep same-ID isolated
/// roots independent, mirroring [`cancel_admission_at`].
///
/// This read is wholly rooted, which is why it gets an `_at` form when the
/// admitting siblings do not. The projection below resolves nothing but the
/// admission queue beneath `runtime_root`, so a rooted status can never report
/// this installation's queue position against another installation's owner.
/// `pin_current`, `admit_current_for`, `activate_installed_generation`,
/// `migrate_legacy_pin`, and `recover_pin` all also publish into the
/// content-addressed pin store, which is deliberately process-global (#7505);
/// rooting only their queue half is exactly the split this campaign forbids.
/// Nothing here touches that store.
pub fn admission_status_at(runtime_root: &Path, request_id: &str) -> Result<Value> {
    fs::create_dir_all(runtime_root).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("create controller runtime directory".to_string()),
        )
    })?;
    admission_status_at_lock_path(&runtime_root.join(ADMISSION_LOCK_DIR), request_id)
}

/// Project the admission view from an already-resolved admission lock path.
///
/// Every durable queue helper in this module is keyed on the lock path, so both
/// entry points converge here rather than rebuilding the projection — and the
/// queued-admission wait loop, which is already handed an explicit lock path,
/// reads its own position through this instead of re-resolving the ambient root.
fn admission_status_at_lock_path(lock_path: &Path, request_id: &str) -> Result<Value> {
    let mut queue = read_admission_queue(lock_path)?;
    // Status reads never rewrite the durable queue. They still hide expired
    // waiters immediately; the next writer compacts them atomically.
    reclaim_stale_admission_entries(lock_path, &mut queue);
    let requests = queue["requests"].as_array().cloned().unwrap_or_default();
    let position = requests
        .iter()
        .position(|request| request["request_id"].as_str() == Some(request_id));
    Ok(json!({
        "state": if queue["owner"]["request_id"].as_str() == Some(request_id) { "admitted" } else if position.is_some() { "waiting" } else { "none" },
        "position": position.map(|index| index + 1),
        "owner": queue["owner"],
        "requested_at_ms": requests.get(position.unwrap_or(usize::MAX)).and_then(|request| request["requested_at_ms"].as_u64()),
        "wait_duration_ms": requests.get(position.unwrap_or(usize::MAX)).and_then(|request| request["requested_at_ms"].as_u64()).map(|then| now_millis().saturating_sub(then)),
    }))
}

/// Remove a waiting request. An owner is intentionally never force-released:
/// the advisory lock remains the authority while a process is alive.
#[cfg(test)]
pub fn cancel_admission(request_id: &str) -> Result<()> {
    cancel_admission_at(&runtime_root()?, request_id)
}

/// Remove a waiting request from an explicitly selected controller-runtime
/// store. Lifecycle stores use this to keep same-ID isolated roots independent.
pub fn cancel_admission_at(runtime_root: &Path, request_id: &str) -> Result<()> {
    fs::create_dir_all(runtime_root).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("create controller runtime directory".to_string()),
        )
    })?;
    let lock_path = runtime_root.join(ADMISSION_LOCK_DIR);
    update_admission_queue(&lock_path, |queue| {
        queue["requests"] = queue["requests"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|request| request["request_id"].as_str() != Some(request_id))
            .collect();
    })
}

/// Publish the current executable as the generation selected for future
/// admissions. Existing records retain their own pinned runtime metadata.
pub fn activate_current_generation() -> Result<Value> {
    let executable = std::env::current_exe().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("resolve controller executable".to_string()),
        )
    })?;
    activate_installed_generation(&executable)
}

/// Publish the executable that installation just verified. This intentionally
/// does not use the upgrading process's executable: after an on-disk swap that
/// process can still be running the previous generation.
pub fn activate_installed_generation(executable: &Path) -> Result<Value> {
    activate_installed_generation_in_root(&runtime_root()?, executable)
}

/// [`activate_installed_generation`] under an already-resolved runtime root.
pub fn activate_installed_generation_in_root(root: &Path, executable: &Path) -> Result<Value> {
    let lock_path = root.join(ADMISSION_LOCK_DIR);
    let _lock = acquire_admission_lock(&lock_path)?;
    let runtime = pin_executable(executable, &activated_executable_identity(executable)?)?;
    validate_pin(&runtime)?;
    write_active_generation(&root.join(ACTIVE_GENERATION_FILE), &runtime)?;
    Ok(runtime)
}

pub fn pinned_executable_for_mutation(
    metadata: &Value,
    current_identity: &str,
) -> Result<Option<PathBuf>> {
    let Some(runtime) = metadata.get(CONTROLLER_RUNTIME_METADATA_KEY) else {
        return Ok(None);
    };
    validate_pin(runtime)?;
    let originating = runtime
        .pointer("/originating/build_identity")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if originating.is_empty() || originating == current_identity {
        return Ok(None);
    }
    let pinned = runtime
        .pointer("/originating/pinned_executable")
        .and_then(Value::as_str)
        .unwrap_or("<pinned-controller-runtime>");
    Ok(Some(PathBuf::from(pinned)))
}

pub fn validate_for_mutation(metadata: &Value, current_identity: &str) -> Result<()> {
    let Some(pinned) = pinned_executable_for_mutation(metadata, current_identity)? else {
        return Ok(());
    };
    let originating = metadata
        .get(CONTROLLER_RUNTIME_METADATA_KEY)
        .and_then(|runtime| runtime.pointer("/originating/build_identity"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    Err(Error::validation_invalid_argument(
        "controller_runtime",
        format!(
            "durable run was created by controller runtime `{originating}`, but this command is `{current_identity}`"
        ),
        Some(current_identity.to_string()),
        Some(vec![format!(
            "Run the lifecycle mutation through the pinned compatible runtime: {} <original homeboy arguments>",
            pinned.display()
        )]),
    ))
}

/// `migrate_legacy_pin` under an already-resolved runtime root.
pub fn migrate_legacy_pin_in_root(root: &Path, runtime: &Value) -> Result<Value> {
    let _lock = acquire_admission_lock(&root.join(ADMISSION_LOCK_DIR))?;
    migrate_legacy_pin_unlocked(runtime)
}

/// [`migrate_legacy_pin_and_persist`] under an already-resolved runtime root.
pub fn migrate_legacy_pin_and_persist_in_root(
    runtime_root: &Path,
    runtime: &Value,
    persist: impl FnOnce(&Value) -> Result<()>,
) -> Result<Value> {
    let _lock = acquire_admission_lock(&runtime_root.join(ADMISSION_LOCK_DIR))?;
    let migrated = migrate_legacy_pin_unlocked(runtime)?;
    if &migrated != runtime {
        persist(&migrated)?;
    }
    Ok(migrated)
}

fn migrate_legacy_pin_unlocked(runtime: &Value) -> Result<Value> {
    let identity =
        required_runtime_string(runtime, "/originating/build_identity", "build identity")?;
    let current = required_runtime_string(
        runtime,
        "/originating/pinned_executable",
        "immutable executable",
    )?;
    let current = Path::new(current);

    // v1 pins predate a content digest. The retained executable is the only
    // trusted migration source; never substitute the current binary or a checkout.
    if runtime.pointer("/originating/sha256").is_none() {
        verify_executable(current, "legacy controller runtime")?;
        verify_self_status_identity(current, identity)?;
        let digest = executable_digest(current)?;
        let destination = pinned_path(identity, &digest)?;
        publish_pin(current, &destination, &digest)?;

        let mut migrated = runtime.clone();
        migrated["schema"] = json!("homeboy/controller-runtime-pin/v2");
        migrated["originating"]["sha256"] = json!(digest);
        migrated["originating"]["pinned_executable"] = json!(destination);
        for field in ["requested", "current", "executed"] {
            if migrated.get(field).is_none() || migrated[field].is_null() {
                migrated[field] = json!(identity);
            }
        }
        validate_pin(&migrated)?;
        return Ok(migrated);
    }

    let digest = required_runtime_string(runtime, "/originating/sha256", "content digest")?;
    let destination = pinned_path(identity, digest)?;
    if current == destination {
        validate_pin(runtime)?;
        return Ok(runtime.clone());
    }

    // Validation includes the digest, executable bit, and advertised identity.
    // Never update durable metadata until the no-clobber publication succeeds.
    validate_pin(runtime)?;
    publish_pin(current, &destination, digest)?;
    let mut migrated = runtime.clone();
    migrated["originating"]["pinned_executable"] = json!(destination);
    validate_pin(&migrated)?;
    Ok(migrated)
}

pub fn validate(runtime: &Value) -> Result<()> {
    validate_pin(runtime)
}

/// `recover_pin` under an already-resolved runtime root.
pub fn recover_pin_in_root(
    root: &Path,
    runtime: &Value,
    artifact: Option<&Path>,
    source: Option<&Path>,
) -> Result<Value> {
    let _lock = acquire_admission_lock(&root.join(ADMISSION_LOCK_DIR))?;
    recover_pin_unlocked(runtime, artifact, source)
}

/// Publish a recovered pin and persist its durable reference under one
/// admission lock, closing the publication-to-record race with cleanup.
pub fn recover_pin_and_persist(
    runtime: &Value,
    artifact: Option<&Path>,
    source: Option<&Path>,
    persist: impl FnOnce(&Value) -> Result<()>,
) -> Result<Value> {
    recover_pin_and_persist_in_root(&runtime_root()?, runtime, artifact, source, persist)
}

/// [`recover_pin_and_persist`] under an already-resolved runtime root.
pub fn recover_pin_and_persist_in_root(
    runtime_root: &Path,
    runtime: &Value,
    artifact: Option<&Path>,
    source: Option<&Path>,
    persist: impl FnOnce(&Value) -> Result<()>,
) -> Result<Value> {
    let _lock = acquire_admission_lock(&runtime_root.join(ADMISSION_LOCK_DIR))?;
    let recovered = recover_pin_unlocked(runtime, artifact, source)?;
    persist(&recovered)?;
    Ok(recovered)
}

fn recover_pin_unlocked(
    runtime: &Value,
    artifact: Option<&Path>,
    source: Option<&Path>,
) -> Result<Value> {
    let identity =
        required_runtime_string(runtime, "/originating/build_identity", "build identity")?;
    let expected = required_runtime_string(runtime, "/originating/sha256", "content digest")?;
    // Recovery never repairs an existing path in place. A corrupted canonical
    // path can still be referenced by another durable record, so this record
    // receives a distinct immutable snapshot after the artifact is verified.
    let destination = recovered_pinned_path(identity, expected)?;
    if let Some(artifact) = artifact {
        verify_artifact(artifact, expected, identity)?;
        publish_pin(artifact, &destination, expected)?;
        let mut recovered = runtime.clone();
        recovered["originating"]["pinned_executable"] = json!(destination);
        validate_pin(&recovered)?;
        return Ok(recovered);
    }
    let revision = runtime
        .pointer("/originating/source/revision")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            identity
                .rsplit_once('+')
                .map(|(_, revision)| revision.to_string())
        })
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "controller_runtime",
                "controller runtime recovery needs recorded source revision",
                Some(identity.to_string()),
                None,
            )
        })?;
    let source = source.ok_or_else(|| {
        Error::validation_invalid_argument(
            "source",
            "controller runtime recovery requires --artifact or --source",
            None,
            None,
        )
    })?;
    let temporary = tempfile::tempdir().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("create controller runtime recovery workspace".to_string()),
        )
    })?;
    let checkout = temporary.path().join("source");
    run_command(
        "git",
        [
            "-C",
            &source.display().to_string(),
            "worktree",
            "add",
            "--detach",
            &checkout.display().to_string(),
            &revision,
        ],
    )?;
    let target =
        crate::cleanup::acquire_shared_cargo_target(&format!("controller-runtime:{revision}"))?;
    let build = Command::new("cargo")
        .args(["build", "--release", "--bin", "homeboy"])
        .env("CARGO_TARGET_DIR", target.target_dir())
        .current_dir(&checkout)
        .status()
        .map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("build controller runtime recovery source".to_string()),
            )
        })?;
    if !build.success() {
        let _ = run_command(
            "git",
            [
                "-C",
                &source.display().to_string(),
                "worktree",
                "remove",
                "--force",
                &checkout.display().to_string(),
            ],
        );
        return Err(Error::validation_invalid_argument(
            "controller_runtime",
            "controller runtime recovery build failed",
            Some(identity.to_string()),
            None,
        ));
    }
    let built = target.target_dir().join("release/homeboy");
    let actual = executable_digest(&built)?;
    if actual != expected {
        let _ = run_command(
            "git",
            [
                "-C",
                &source.display().to_string(),
                "worktree",
                "remove",
                "--force",
                &checkout.display().to_string(),
            ],
        );
        return Err(Error::validation_invalid_argument(
            "controller_runtime",
            format!(
                "recovered controller runtime hash does not match durable pin: expected {expected}"
            ),
            Some(built.display().to_string()),
            None,
        ));
    }
    verify_artifact(&built, expected, identity)?;
    publish_pin(&built, &destination, expected)?;
    let _ = run_command(
        "git",
        [
            "-C",
            &source.display().to_string(),
            "worktree",
            "remove",
            "--force",
            &checkout.display().to_string(),
        ],
    );
    let mut recovered = runtime.clone();
    recovered["originating"]["pinned_executable"] = json!(destination);
    validate_pin(&recovered)?;
    Ok(recovered)
}

fn runtime_root() -> Result<PathBuf> {
    runtime_root_in(&paths::PathRoots::from_environment()?.data().to_path_buf())
}

/// [`runtime_root`] below an already-resolved data root.
///
/// The admission lock this root carries is machine-global when the root is
/// ambient: two tests marking a run running serialize against each other even
/// with separate stores. Supplying the root is what makes that lock local to
/// the caller's installation (#7505).
pub fn runtime_root_in(data_root: &Path) -> Result<PathBuf> {
    let root = paths::controller_runtimes_store_in_root(data_root);
    fs::create_dir_all(&root).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("create controller runtime directory".to_string()),
        )
    })?;
    Ok(root)
}

fn write_active_generation(path: &Path, runtime: &Value) -> Result<()> {
    let temporary = path.with_extension("tmp");
    fs::write(
        &temporary,
        serde_json::to_vec(runtime)
            .map_err(|error| Error::internal_json(error.to_string(), None))?,
    )
    .map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("write active controller generation".to_string()),
        )
    })?;
    fs::rename(temporary, path).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("publish active controller generation".to_string()),
        )
    })
}

/// Acquire admission for an uncoordinated caller: cleanup, generation
/// activation, and pin migration/recovery carry no durable request ID, so they
/// cannot join the FIFO queue that cook submissions use.
///
/// They must still not fast-fail the instant a parallel cook wave holds
/// admission. Bounded queueing is strictly better than an immediate retryable
/// error the operator has to re-run by hand (#9373): wait out a normal
/// admission critical section (which is short — selection plus durable record
/// creation), then surface the contention with a named owner.
fn acquire_admission_lock(path: &Path) -> Result<AdmissionLock> {
    acquire_admission_lock_bounded(path, admission_busy_wait())
}

fn acquire_admission_lock_bounded(path: &Path, wait: Duration) -> Result<AdmissionLock> {
    let request_id = format!("controller-{}", Uuid::new_v4());
    let timings = AdmissionTimings::load();
    let started = std::time::Instant::now();
    loop {
        match acquire_admission_lock_for(path, &request_id) {
            Ok(lock) => return Ok(lock),
            Err(error) if error.retryable == Some(true) => {
                if started.elapsed() >= wait {
                    return Err(error);
                }
                std::thread::sleep(timings.poll.min(wait));
            }
            Err(error) => return Err(error),
        }
    }
}

fn acquire_admission_lock_for(path: &Path, request_id: &str) -> Result<AdmissionLock> {
    reject_legacy_admission_lock(path)?;
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("open controller admission lock".to_string()),
            )
        })?;

    let Some(process_guard) = try_acquire_admission_process_guard(path) else {
        return Err(admission_busy_error(path));
    };
    let observed_owner = read_admission_owner(path)?;
    replace_admission_owner_after_snapshot_for_test(path)?;
    let acquired = file.try_lock_exclusive().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("acquire controller admission lock".to_string()),
        )
    })?;
    if !acquired {
        return Err(admission_busy_error(path));
    }
    let current_owner = read_admission_owner(path)?;
    if current_owner != observed_owner {
        return Err(Error::internal_unexpected(
            "controller admission owner changed while reclaiming stale ownership",
        )
        .with_retryable(true));
    }
    if let Some(owner) = current_owner.as_ref() {
        ensure_admission_owner_is_recoverable(owner)?;
    }
    let token = Uuid::new_v4().to_string();
    let owner = admission_owner_record(&token);
    file.set_len(0).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("clear controller admission owner record".to_string()),
        )
    })?;
    file.write_all(&serde_json::to_vec(&owner).map_err(|error| {
        Error::internal_json(
            error.to_string(),
            Some("serialize controller admission owner".to_string()),
        )
    })?)
    .map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("write controller admission owner record".to_string()),
        )
    })?;
    file.sync_data().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("sync controller admission owner record".to_string()),
        )
    })?;
    Ok(AdmissionLock {
        path: path.to_path_buf(),
        token,
        request_id: request_id.to_string(),
        _process_guard: process_guard,
        file,
    })
}

fn read_admission_owner(path: &Path) -> Result<Option<Value>> {
    let bytes = fs::read(path).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("read controller admission owner record".to_string()),
        )
    })?;
    if bytes.is_empty() {
        return Ok(None);
    }
    let owner: Value = serde_json::from_slice(&bytes).map_err(|error| {
        Error::validation_invalid_json(
            error,
            Some("parse controller admission owner record".to_string()),
            None,
        )
    })?;
    if !owner.is_object() {
        return Err(Error::validation_invalid_argument(
            "controller_admission",
            "controller admission owner record must be an object",
            None,
            None,
        ));
    }
    Ok(Some(owner))
}

fn ensure_admission_owner_is_recoverable(owner: &Value) -> Result<()> {
    ensure_admission_owner_is_recoverable_with(owner, crate::process::process_identity_state)
}

fn ensure_admission_owner_is_recoverable_with(
    owner: &Value,
    inspect_process: impl FnOnce(u32, Option<u64>) -> crate::process::ProcessIdentityState,
) -> Result<()> {
    if owner["schema"].as_str() != Some(ADMISSION_OWNER_SCHEMA)
        || owner["token"].as_str().is_none_or(str::is_empty)
    {
        return Err(Error::validation_invalid_argument(
            "controller_admission",
            "controller admission owner record is malformed or uses an unsupported schema",
            None,
            None,
        ));
    }
    let Some(pid) = owner["pid"].as_u64() else {
        // Older durable records can name a fence without a local process. They
        // cannot protect a live local owner, so the advisory-lock CAS may replace them.
        return Ok(());
    };
    let pid = u32::try_from(pid).map_err(|_| {
        Error::validation_invalid_argument(
            "controller_admission",
            "controller admission owner record has an invalid PID",
            None,
            None,
        )
    })?;
    let starttime = match owner.get("linux_starttime_ticks") {
        Some(Value::Null) => None,
        Some(value) => match value.as_u64() {
            Some(value) => Some(value),
            None => {
                return Err(Error::validation_invalid_argument(
                    "controller_admission",
                    "controller admission owner record has an invalid process identity",
                    None,
                    None,
                ))
            }
        },
        None => {
            return Err(Error::validation_invalid_argument(
                "controller_admission",
                "controller admission owner record has an invalid process identity",
                None,
                None,
            ));
        }
    };
    match inspect_process(pid, starttime) {
        crate::process::ProcessIdentityState::Dead => Ok(()),
        crate::process::ProcessIdentityState::Live => Err(admission_owner_reclaim_error(
            "controller admission owner process is still live",
        )),
        crate::process::ProcessIdentityState::IdentityMismatch => {
            Err(admission_owner_reclaim_error(
                "controller admission owner PID was reused by a different process",
            ))
        }
        crate::process::ProcessIdentityState::Unverifiable => Err(admission_owner_reclaim_error(
            "controller admission owner liveness cannot be verified",
        )),
    }
}

fn admission_owner_reclaim_error(message: &str) -> Error {
    Error::internal_unexpected(message).with_retryable(true)
}

#[cfg(test)]
fn replace_admission_owner_after_snapshot_for_test(path: &Path) -> Result<()> {
    if let Some(owner) = TEST_ADMISSION_OWNER_CAS_REPLACEMENT
        .get_or_init(|| Mutex::new(None))
        .lock()
        .expect("admission owner test hook is not poisoned")
        .take()
    {
        fs::write(
            path,
            serde_json::to_vec(&owner)
                .map_err(|error| Error::internal_json(error.to_string(), None))?,
        )
        .map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("replace controller admission owner record for test".to_string()),
            )
        })?;
    }
    Ok(())
}

#[cfg(not(test))]
fn replace_admission_owner_after_snapshot_for_test(_path: &Path) -> Result<()> {
    Ok(())
}

fn admission_busy_error(path: &Path) -> Error {
    let mut error = Error::internal_unexpected(format!(
        "controller generation admission is currently owned by {}",
        admission_owner_summary(path)
    ))
    .with_retryable(true);
    error.details["controller_admission"] = admission_contention_evidence(path);
    error
}

/// Structured contention evidence for an admission failure.
///
/// The rendered summary is the operator-facing sentence; this is the
/// machine-readable form durable records and follow-up commands can join on, so
/// a failed admission attempt is inspectable after the process exits (#9373).
fn admission_contention_evidence(path: &Path) -> Value {
    let queue = read_admission_queue(path).unwrap_or_else(
        |_| json!({ "schema": ADMISSION_QUEUE_SCHEMA, "requests": [], "owner": null }),
    );
    let waiting = queue["requests"]
        .as_array()
        .map(|requests| requests.len())
        .unwrap_or(0);
    json!({
        "owner_summary": admission_owner_summary(path),
        "owner": queue["owner"],
        "waiting_requests": waiting,
        "advisory_lock_held": admission_lock_is_held(path),
        "queueing": "controller admission is FIFO; concurrent requests wait their turn instead of racing",
    })
}

#[cfg(test)]
fn acquire_admission_lock_with_retry(
    path: &Path,
    attempts: usize,
    retry: Duration,
) -> Result<AdmissionLock> {
    let started = std::time::Instant::now();
    for _ in 0..attempts {
        match acquire_admission_lock_for(path, "test-admission") {
            Ok(lock) => return Ok(lock),
            Err(error) if error.retryable == Some(true) => std::thread::sleep(retry),
            Err(error) => return Err(error),
        }
    }
    Err(Error::validation_invalid_argument(
        "controller_admission",
        format!(
            "controller generation admission timed out; waited {}ms; current owner: {}",
            started.elapsed().as_millis(),
            admission_owner_summary(path)
        ),
        None,
        None,
    ))
}

fn acquire_queued_admission_lock(
    path: &Path,
    request_id: &str,
    cancellation_requested: &impl Fn() -> Result<bool>,
) -> Result<AdmissionLock> {
    acquire_queued_admission_lock_with_timeout(
        path,
        request_id,
        admission_queue_wait_timeout(),
        cancellation_requested,
    )
}

fn acquire_queued_admission_lock_with_timeout(
    path: &Path,
    request_id: &str,
    wait_timeout: Duration,
    cancellation_requested: &impl Fn() -> Result<bool>,
) -> Result<AdmissionLock> {
    let timings = AdmissionTimings::load();
    let started = std::time::Instant::now();
    let mut last_heartbeat = std::time::Instant::now();
    let mut backoff = timings.poll;
    let mut observed_position = None;
    loop {
        // Read the queue this wait was handed a path into. Reading the ambient
        // root here would let a waiter enqueued under an explicit store poll a
        // different installation's queue for its own position.
        let status = admission_status_at_lock_path(path, request_id)?;
        if status["state"] == "none" {
            return Err(
                Error::internal_unexpected("controller admission request was cancelled")
                    .with_retryable(true),
            );
        }
        let position = status["position"].as_u64();
        if started.elapsed() >= wait_timeout {
            return Err(admission_wait_timeout_error(
                path,
                request_id,
                started.elapsed(),
                wait_timeout,
                position,
            )?);
        }
        if last_heartbeat.elapsed() >= timings.heartbeat {
            heartbeat_admission_waiter(path, request_id)?;
            last_heartbeat = std::time::Instant::now();
        }
        // The queue moved, so this waiter is closer to its turn: return to a
        // tight poll rather than sleeping through the handoff it is waiting for.
        if position != observed_position {
            backoff = timings.poll;
            observed_position = position;
        }
        let at_head = position == Some(1);
        if at_head {
            wait_at_admission_head();
            match acquire_admission_lock_for(path, request_id) {
                Ok(lock) => {
                    if claim_admission_owner(path, request_id, &lock.token, cancellation_requested)?
                    {
                        return Ok(lock);
                    }
                    drop(lock);
                    return Err(admission_cancelled_error(request_id));
                }
                Err(error) if error.retryable == Some(true) => (),
                Err(error) => return Err(error),
            }
        }
        // The head polls at the floor: it is next to be admitted and the
        // critical section it is waiting on is short (selection plus durable
        // record creation). Only a waiter that is not yet at the head, and
        // whose position has not moved, backs off — that is the waiter for whom
        // re-reading the durable queue is pure cost.
        let sleep = if at_head { timings.poll } else { backoff };
        // Never overshoot the deadline: backing off must not make the reported
        // wait longer than the timeout the operator configured.
        std::thread::sleep(sleep.min(wait_timeout.saturating_sub(started.elapsed())));
        if !at_head {
            backoff = next_admission_backoff(backoff, &timings);
        }
    }
}

/// Build the error for a queued request that never reached the head.
///
/// This is the silent-partial-failure path: fan out a wave larger than the
/// admission lock can drain and the waiters that lose simply return
/// `retryable: true` with nothing actually retrying them. The error's job is
/// therefore to be impossible to misread — how long it waited, how many
/// requests were ahead of it, who held the lock, the command that resumes it,
/// and the knob that widens the window.
///
/// Deliberately *not* auto-retried. Re-enqueueing after a timeout appends to
/// the tail, so the waiter that already waited longest would be sent to the
/// back — punishing seniority under exactly the sustained contention that
/// produced the timeout, and risking indefinite starvation on a global lock. A
/// retry belongs one layer up, where a durable run record makes it observable
/// and countable instead of an invisible in-process loop.
fn admission_wait_timeout_error(
    path: &Path,
    request_id: &str,
    waited: Duration,
    wait_timeout: Duration,
    position: Option<u64>,
) -> Result<Error> {
    // Name the holder before the waiter is removed: an expired wait whose
    // diagnostic cannot say who held admission is unactionable (#9373).
    let owner = admission_owner_summary(path);
    let evidence = admission_contention_evidence(path);
    let queue_depth = evidence["waiting_requests"].as_u64().unwrap_or_default();
    remove_admission_request(path, request_id)?;

    let ahead = position.map(|position| position.saturating_sub(1));
    let ahead_summary = match ahead {
        Some(1) => "1 other waiter".to_string(),
        Some(ahead) => format!("{ahead} other waiters"),
        None => "an unknown number of other waiters".to_string(),
    };
    let retry_command = format!("homeboy agent-task retry {request_id} --run");
    // The millisecond form of the timeout is retained verbatim so existing
    // operator greps and assertions on this message keep matching.
    let mut error = Error::internal_unexpected(format!(
        "controller generation admission queue wait exceeded {}ms; request `{request_id}` waited {} behind {ahead_summary} without ever reaching the head of the queue ({queue_depth} request(s) in the queue including this one); current owner: {owner}",
        wait_timeout.as_millis(),
        format_admission_duration(waited),
    ))
    .with_retryable(true)
    .with_hint(format!(
        "This request was never admitted, so no task work was dispatched for it. Resume it with: {retry_command} (for cook submissions the admission request ID is the agent-task run ID)."
    ))
    .with_hint(
        "Admission is FIFO: re-running the identical command queues behind the current owner instead of racing it.",
    )
    .with_hint(format!(
        "If a whole submission wave times out, the wave is larger than admission can drain in {}. Widen it with: homeboy config set /controller_admission/queue_wait_timeout_ms <milliseconds>",
        format_admission_duration(wait_timeout),
    ));
    error.details["controller_admission"] = evidence;
    error.details["request_id"] = json!(request_id);
    error.details["waited_ms"] = json!(waited.as_millis() as u64);
    error.details["wait_timeout_ms"] = json!(wait_timeout.as_millis() as u64);
    error.details["waiters_ahead"] = json!(ahead);
    error.details["queue_depth"] = json!(queue_depth);
    error.details["retry_command"] = json!(retry_command);
    // Explicit: nothing retried this for you.
    error.details["automatic_retry"] = json!(false);
    Ok(error)
}

fn admission_cancelled_error(request_id: &str) -> Error {
    let mut error = Error::internal_unexpected("controller admission request was cancelled")
        .with_retryable(true);
    error.details = json!({ "request_id": request_id, "outcome": "cancelled" });
    error
}

fn wait_at_admission_head() {
    #[cfg(test)]
    if let Some(barrier) = TEST_ADMISSION_HEAD_BARRIER
        .get_or_init(|| Mutex::new(None))
        .lock()
        .expect("admission test hook is not poisoned")
        .clone()
    {
        barrier.wait();
        barrier.wait();
    }
}

fn claim_admission_owner(
    lock_path: &Path,
    request_id: &str,
    token: &str,
    cancellation_requested: &impl Fn() -> Result<bool>,
) -> Result<bool> {
    let lock_file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(queue_lock_path(lock_path))
        .map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("open controller admission queue lock".to_string()),
            )
        })?;
    lock_file.lock_exclusive().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("lock controller admission queue".to_string()),
        )
    })?;
    let mut queue = read_admission_queue(lock_path)?;
    reclaim_stale_admission_entries(lock_path, &mut queue);
    let is_head = queue["requests"]
        .as_array()
        .and_then(|requests| requests.first())
        .is_some_and(|request| {
            request["request_id"].as_str() == Some(request_id)
                && request["state"].as_str() == Some("waiting")
        });
    if !is_head || cancellation_requested()? {
        let _ = lock_file.unlock();
        return Ok(false);
    }
    let now = now_millis();
    let owner = admission_owner_record(token);
    queue["owner"] = json!({
        "request_id": request_id,
        "pid": owner["pid"],
        "linux_starttime_ticks": owner["linux_starttime_ticks"],
        "heartbeat_at_ms": now,
        "lease_expires_at_ms": now + admission_queue_lease().as_millis() as u64,
        "advisory_lock": true,
    });
    write_admission_queue(lock_path, &queue)?;
    let _ = lock_file.unlock();
    Ok(true)
}

fn queue_path(lock_path: &Path) -> PathBuf {
    lock_path.with_file_name(ADMISSION_QUEUE_FILE)
}

fn queue_lock_path(lock_path: &Path) -> PathBuf {
    lock_path.with_file_name(ADMISSION_QUEUE_LOCK_FILE)
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn read_admission_queue(lock_path: &Path) -> Result<Value> {
    let path = queue_path(lock_path);
    Ok(fs::read(&path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_else(
            || json!({ "schema": ADMISSION_QUEUE_SCHEMA, "requests": [], "owner": null }),
        ))
}

fn update_admission_queue(lock_path: &Path, mutate: impl FnOnce(&mut Value)) -> Result<()> {
    let lock_file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(queue_lock_path(lock_path))
        .map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("open controller admission queue lock".to_string()),
            )
        })?;
    lock_file.lock_exclusive().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("lock controller admission queue".to_string()),
        )
    })?;
    let mut queue = read_admission_queue(lock_path)?;
    reclaim_stale_admission_entries(lock_path, &mut queue);
    mutate(&mut queue);
    write_admission_queue(lock_path, &queue)?;
    let _ = lock_file.unlock();
    Ok(())
}

fn write_admission_queue(lock_path: &Path, queue: &Value) -> Result<()> {
    let temporary = queue_path(lock_path).with_extension("tmp");
    fs::write(
        &temporary,
        serde_json::to_vec(&queue)
            .map_err(|error| Error::internal_json(error.to_string(), None))?,
    )
    .map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("write controller admission queue".to_string()),
        )
    })?;
    fs::rename(temporary, queue_path(lock_path)).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("publish controller admission queue".to_string()),
        )
    })?;
    Ok(())
}

fn enqueue_admission_request(lock_path: &Path, request_id: &str) -> Result<()> {
    update_admission_queue(lock_path, |queue| {
        let requests = queue["requests"]
            .as_array_mut()
            .expect("queue requests initialized");
        if !requests
            .iter()
            .any(|request| request["request_id"].as_str() == Some(request_id))
        {
            let now = now_millis();
            requests.push(json!({ "request_id": request_id, "state": "waiting", "requested_at_ms": now, "heartbeat_at_ms": now, "lease_expires_at_ms": now + admission_queue_lease().as_millis() as u64 }));
        }
    })
}

fn remove_admission_request(lock_path: &Path, request_id: &str) -> Result<()> {
    update_admission_queue(lock_path, |queue| {
        queue["requests"] = queue["requests"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|request| request["request_id"].as_str() != Some(request_id))
            .collect();
    })
}

fn reclaim_stale_admission_entries(lock_path: &Path, queue: &mut Value) {
    reclaim_stale_admission_waiters(queue);
    let Some(owner) = queue["owner"].as_object() else {
        return;
    };
    let expired = owner["lease_expires_at_ms"]
        .as_u64()
        .is_some_and(|expires| expires <= now_millis());
    let owner_is_alive = owner["pid"]
        .as_u64()
        .and_then(|pid| u32::try_from(pid).ok())
        .is_some_and(crate::process::pid_is_running);
    // The kernel advisory lock is authoritative while a controller is alive;
    // the lease merely makes a crashed owner observable and reclaimable.
    if expired && !owner_is_alive && !admission_lock_is_held(lock_path) {
        queue["owner"] = Value::Null;
    }
}

fn reclaim_stale_admission_waiters(queue: &mut Value) {
    let now = now_millis();
    if let Some(requests) = queue["requests"].as_array_mut() {
        requests.retain(|request| {
            request["state"] == "cancelled"
                || request["lease_expires_at_ms"]
                    .as_u64()
                    .is_none_or(|expires| expires > now)
        });
    }
}

fn admission_lock_is_held(path: &Path) -> bool {
    let Ok(file) = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
    else {
        return true;
    };
    match file.try_lock_exclusive() {
        Ok(true) => {
            let _ = file.unlock();
            false
        }
        Ok(false) | Err(_) => true,
    }
}

fn heartbeat_admission_waiter(path: &Path, request_id: &str) -> Result<()> {
    update_admission_queue(path, |queue| {
        if let Some(request) = queue["requests"].as_array_mut().and_then(|requests| {
            requests
                .iter_mut()
                .find(|request| request["request_id"].as_str() == Some(request_id))
        }) {
            let now = now_millis();
            request["heartbeat_at_ms"] = json!(now);
            request["lease_expires_at_ms"] =
                json!(now + admission_queue_lease().as_millis() as u64);
        }
    })
}

fn heartbeat_admission_owner(path: &Path, request_id: &str, runtime: Option<&Value>) -> Result<()> {
    update_admission_queue(path, |queue| {
        if queue["owner"]["request_id"].as_str() == Some(request_id) {
            let now = now_millis();
            queue["owner"]["heartbeat_at_ms"] = json!(now);
            queue["owner"]["lease_expires_at_ms"] =
                json!(now + admission_queue_lease().as_millis() as u64);
            if let Some(runtime) = runtime {
                queue["owner"]["runtime"] = runtime.clone();
                queue["owner"]["runtime_identity"] =
                    runtime["originating"]["build_identity"].clone();
                queue["owner"]["controller_generation"] =
                    runtime["originating"]["build_identity"].clone();
            }
        }
    })
}

fn try_acquire_admission_process_guard(path: &Path) -> Option<MutexGuard<'static, ()>> {
    let guard = {
        let mut guards = ADMISSION_LOCK_PROCESS_GUARDS
            .get_or_init(|| Mutex::new(BTreeMap::new()))
            .lock()
            .expect("controller admission process guard registry is not poisoned");
        *guards
            .entry(path.to_path_buf())
            .or_insert_with(|| Box::leak(Box::new(Mutex::new(()))))
    };
    guard.try_lock().ok()
}

fn admission_owner_record(token: &str) -> Value {
    let pid = std::process::id();
    let starttime_ticks = crate::process::linux_process_starttime_ticks(pid)
        .ok()
        .flatten();
    json!({
        "schema": ADMISSION_OWNER_SCHEMA,
        "token": token,
        "pid": pid,
        "linux_starttime_ticks": starttime_ticks,
    })
}

fn admission_owner_token(path: &Path) -> Option<String> {
    serde_json::from_slice::<Value>(&fs::read(path).ok()?)
        .ok()?
        .get("token")?
        .as_str()
        .map(str::to_string)
}

/// Describe who currently holds controller-generation admission.
///
/// An admission error that cannot name its holder is unactionable by
/// construction: `owned by unavailable` told operators nothing about whether to
/// wait, retry, or repair (#9373). Resolution is therefore layered and always
/// ends in a concrete, actionable state:
///
/// 1. the published owner record, including a verified liveness verdict;
/// 2. the durable admission queue, which names the owning request even while
///    the owner record is being rewritten between claim and publication;
/// 3. the observable advisory-lock state, which distinguishes "held by a
///    process that has not published ownership yet" from "free, retry now".
fn admission_owner_summary(path: &Path) -> String {
    admission_owner_record_summary(path)
        .or_else(|| admission_queue_owner_summary(path))
        .unwrap_or_else(|| admission_lock_state_summary(path))
}

fn admission_owner_record_summary(path: &Path) -> Option<String> {
    let bytes = fs::read(path).ok()?;
    if bytes.is_empty() {
        return None;
    }
    let owner = serde_json::from_slice::<Value>(&bytes).ok()?;
    let token = owner.get("token").and_then(Value::as_str)?;
    let starttime = owner.get("linux_starttime_ticks").and_then(Value::as_u64);
    let Some(pid) = owner
        .get("pid")
        .and_then(Value::as_u64)
        .and_then(|pid| u32::try_from(pid).ok())
    else {
        // A legacy record fences a generation without naming a local process.
        // It cannot protect a live owner, so the next attempt reclaims it.
        return Some(format!(
            "token={token} (legacy owner record with no PID; no live local process holds admission, so the next attempt reclaims it)"
        ));
    };
    let liveness = admission_owner_liveness(pid, starttime);
    Some(match starttime {
        Some(starttime) => {
            format!("pid={pid} ({liveness}), linux_starttime_ticks={starttime}, token={token}")
        }
        None => format!("pid={pid} ({liveness}), token={token}"),
    })
}

fn admission_owner_liveness(pid: u32, starttime: Option<u64>) -> &'static str {
    match crate::process::process_identity_state(pid, starttime) {
        crate::process::ProcessIdentityState::Live => "live; wait for it to release admission",
        crate::process::ProcessIdentityState::Dead => {
            "already exited; the next attempt reclaims admission"
        }
        crate::process::ProcessIdentityState::IdentityMismatch => {
            "PID reused by a different process; the next attempt reclaims admission"
        }
        crate::process::ProcessIdentityState::Unverifiable => {
            "liveness unverifiable on this platform; admission stays protected until the owner record is reclaimed"
        }
    }
}

/// Name the owner recorded in the durable queue when the owner record itself is
/// absent or mid-rewrite. The queue survives the owning process, so it is the
/// only evidence that can name a holder across a controller restart.
fn admission_queue_owner_summary(path: &Path) -> Option<String> {
    let queue = read_admission_queue(path).ok()?;
    let owner = queue.get("owner")?.as_object()?;
    let request_id = owner.get("request_id").and_then(Value::as_str)?;
    let Some(pid) = owner
        .get("pid")
        .and_then(Value::as_u64)
        .and_then(|pid| u32::try_from(pid).ok())
    else {
        return Some(format!(
            "admission request `{request_id}` (durable queue owner with no recorded PID; the queue entry outlives its process, so cancelling or completing that request releases admission)"
        ));
    };
    let liveness = admission_owner_liveness(
        pid,
        owner.get("linux_starttime_ticks").and_then(Value::as_u64),
    );
    Some(format!(
        "admission request `{request_id}`, pid={pid} ({liveness})"
    ))
}

fn admission_lock_state_summary(path: &Path) -> String {
    if admission_lock_is_held(path) {
        "a controller that holds the advisory lock but has not published its owner record yet (it is still running, so retry — concurrent requests queue in FIFO order)".to_string()
    } else {
        "no live owner (the advisory lock is free and the owner record is absent or unreadable, so retry immediately)".to_string()
    }
}

fn reject_legacy_admission_lock(path: &Path) -> Result<()> {
    if !path.is_dir() {
        return Ok(());
    }
    Err(Error::validation_invalid_argument(
        "controller_admission",
        format!(
            "legacy controller admission lock directory exists at {}; it may be held by an older controller. Stop confirmed old controllers, then remove the abandoned directory explicitly before retrying",
            path.display()
        ),
        Some(path.display().to_string()),
        None,
    ))
}

fn validate_pin(runtime: &Value) -> Result<()> {
    let pinned = runtime
        .pointer("/originating/pinned_executable")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "controller_runtime",
                "controller runtime pin has no immutable executable",
                None,
                None,
            )
        })?;
    let expected = runtime
        .pointer("/originating/sha256")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "controller_runtime",
                "controller runtime pin has no content digest",
                Some(pinned.to_string()),
                None,
            )
        })?;
    let path = Path::new(pinned);
    let metadata = fs::metadata(path).map_err(|_| {
        Error::validation_invalid_argument(
            "controller_runtime",
            format!("pinned controller executable is missing: {pinned}"),
            Some(pinned.to_string()),
            None,
        )
    })?;
    if !metadata.is_file() || !is_executable(&metadata) {
        return Err(Error::validation_invalid_argument(
            "controller_runtime",
            format!("pinned controller executable is not executable: {pinned}"),
            Some(pinned.to_string()),
            None,
        ));
    }
    let actual = test_candidate_or_executable_digest(path)?;
    if actual != expected {
        return Err(Error::validation_invalid_argument(
            "controller_runtime",
            format!(
                "pinned controller executable hash mismatch: expected {expected}, found {actual}"
            ),
            Some(pinned.to_string()),
            None,
        ));
    }
    let identity =
        required_runtime_string(runtime, "/originating/build_identity", "build identity")?;
    verify_self_identity(path, identity, Some(&actual))?;
    Ok(())
}

fn verify_artifact(path: &Path, expected: &str, identity: &str) -> Result<()> {
    verify_executable(path, "recovery artifact")?;
    let actual = executable_digest(path)?;
    if actual != expected {
        return Err(Error::validation_invalid_argument(
            "controller_runtime",
            format!("recovery artifact hash mismatch: expected {expected}, found {actual}"),
            Some(path.display().to_string()),
            None,
        ));
    }
    verify_self_identity(path, identity, Some(&actual))
}

fn verify_executable(path: &Path, label: &str) -> Result<()> {
    let metadata = fs::metadata(path).map_err(|_| {
        Error::validation_invalid_argument(
            "controller_runtime",
            format!("{label} is missing: {}", path.display()),
            Some(path.display().to_string()),
            None,
        )
    })?;
    if !metadata.is_file() || !is_executable(&metadata) {
        return Err(Error::validation_invalid_argument(
            "controller_runtime",
            format!("{label} is not executable: {}", path.display()),
            Some(path.display().to_string()),
            None,
        ));
    }
    Ok(())
}

fn verify_self_identity(path: &Path, expected: &str, verified_digest: Option<&str>) -> Result<()> {
    let actual = executable_identity(path, verified_digest)?;
    if actual == expected {
        return Ok(());
    }
    Err(Error::validation_invalid_argument(
        "controller_runtime",
        format!(
            "pinned controller executable build identity mismatch: expected {expected}, found {actual}"
        ),
        Some(path.display().to_string()),
        None,
    ))
}

/// v1 records have no digest, so require the historical executable's full
/// status report to advertise the identity retained by the durable record.
fn verify_self_status_identity(path: &Path, expected: &str) -> Result<()> {
    let output = Command::new(path)
        .args(["self", "status"])
        .output()
        .map_err(|error| {
            Error::validation_invalid_argument(
                "controller_runtime",
                format!("legacy controller runtime status check failed: {error}"),
                Some(path.display().to_string()),
                None,
            )
        })?;
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let actual = serde_json::from_str::<Value>(&stdout)
        .ok()
        .and_then(|value| {
            value
                .pointer("/data/active_build_identity/display")
                .or_else(|| value.pointer("/active_build_identity/display"))
                .and_then(Value::as_str)
                .map(str::to_string)
        });
    if !output.status.success() || actual.is_none() {
        return Err(Error::validation_invalid_argument(
            "controller_runtime",
            format!("legacy controller runtime status check returned invalid output: {stdout}"),
            Some(path.display().to_string()),
            None,
        ));
    }
    let actual = actual.expect("status identity was checked above");
    if actual == expected {
        return Ok(());
    }
    Err(Error::validation_invalid_argument(
        "controller_runtime",
        format!(
            "legacy controller runtime build identity mismatch: expected {expected}, found {actual}"
        ),
        Some(path.display().to_string()),
        None,
    ))
}

fn executable_identity(path: &Path, verified_digest: Option<&str>) -> Result<String> {
    #[cfg(not(any(test, feature = "test-support")))]
    let _ = verified_digest;
    #[cfg(any(test, feature = "test-support"))]
    if let Some(identity) = test_controller_identity(path, verified_digest) {
        return identity;
    }
    // Never spawn our own bytes to ask who we are. A candidate whose contents
    // are identical to the running executable *is* the running executable, so
    // its identity is the one compiled into this process — proven by digest
    // rather than self-reported, which is strictly stronger than asking it.
    //
    // This also removes a hazard: the candidate is not always a controller.
    // Under `cargo test` the running executable is a libtest binary, and a pin
    // taken from it is a byte-identical copy. Executing that copy makes libtest
    // parse `self identity` as two test *name filters* rather than a
    // subcommand, run every test matching them, and return their output as the
    // "identity" (#12226).
    if is_current_executable_content(path, verified_digest) {
        return Ok(crate::build_identity::current().display);
    }
    let output = Command::new(path)
        .args(["self", "identity"])
        .output()
        .map_err(|error| {
            Error::validation_invalid_argument(
                "controller_runtime",
                format!("pinned controller executable identity check failed: {error}"),
                Some(path.display().to_string()),
                None,
            )
        })?;
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let actual = serde_json::from_str::<Value>(&stdout)
        .ok()
        .and_then(|value| {
            value
                .pointer("/data/display")
                .and_then(Value::as_str)
                .map(str::to_string)
        });
    if !output.status.success() || actual.is_none() {
        return Err(Error::validation_invalid_argument(
            "controller_runtime",
            format!(
                "pinned controller executable identity check returned invalid output: {}",
                bounded_identity_output(&stdout)
            ),
            Some(path.display().to_string()),
            None,
        ));
    }
    Ok(actual.expect("identity was checked above"))
}

/// Whether `path` is the executable this process is running from.
///
/// Compared by canonical path so a pin reached through a symlink still resolves
/// to the same file. An unresolvable path is treated as "not us", which keeps
/// the probe fail-closed: the worst case is the pre-existing spawn.
fn is_current_executable(path: &Path) -> bool {
    let Ok(current) = std::env::current_exe() else {
        return false;
    };
    match (path.canonicalize(), current.canonicalize()) {
        (Ok(candidate), Ok(current)) => candidate == current,
        _ => false,
    }
}

/// SHA-256 of the executable this process is running from, hashed at most once.
///
/// Pins are content-addressed copies, so identity questions about them reduce
/// to "are these our bytes?".
///
/// Deliberately hashed outside [`executable_digest`]'s memo. That memo is keyed
/// by observed file identity precisely so it *expires* when a pin is replaced
/// underneath it — that expiry is the reason pins are validated at all. Our own
/// executable is a different question: it cannot change under this process, so
/// it is cached for the process lifetime and kept out of a cache whose whole
/// job is to notice change.
fn current_executable_digest() -> Option<&'static str> {
    static CURRENT_EXECUTABLE_DIGEST: OnceLock<Option<String>> = OnceLock::new();
    CURRENT_EXECUTABLE_DIGEST
        .get_or_init(|| {
            let current = std::env::current_exe().ok()?;
            content_hash::sha256_file(&current).ok()
        })
        .as_deref()
}

/// Whether `path` holds the same bytes as the running executable.
///
/// Path equality is only the fast path: a controller-runtime pin is a *copy*
/// living under a content-addressed directory, so it is never the same path as
/// the executable it was taken from. Any failure to establish this answers
/// "no", leaving the pre-existing spawn as the fallback.
fn is_current_executable_content(path: &Path, verified_digest: Option<&str>) -> bool {
    if is_current_executable(path) {
        return true;
    }
    let Some(current_digest) = current_executable_digest() else {
        return false;
    };
    // Reuse the digest the caller already computed and checked when it has one;
    // pin verification hashes the candidate immediately before asking for its
    // identity, so the common path adds no work at all.
    //
    // Without one, hash directly rather than through `executable_digest`: this
    // is an identity question about a candidate, not a pin whose memo must
    // expire when it is replaced underneath us.
    match verified_digest {
        Some(digest) => digest == current_digest,
        None => content_hash::sha256_file(path).is_ok_and(|digest| digest == current_digest),
    }
}

/// Cap untrusted probe output before it becomes an error message.
///
/// This output is whatever an arbitrary executable wrote, and the error
/// carrying it is serialized into durable run records. An unbounded copy turned
/// one failed probe into a ~40 KB diagnostic that buried its own cause.
fn bounded_identity_output(stdout: &str) -> String {
    const LIMIT: usize = 512;
    if stdout.len() <= LIMIT {
        return stdout.to_string();
    }
    let mut end = LIMIT;
    while end > 0 && !stdout.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}... [truncated, {} bytes total]",
        &stdout[..end],
        stdout.len()
    )
}

/// Test-support uses a copied libtest executable as its fixture. The contract
/// is limited to byte-identical copies of its explicit source executable, so
/// arbitrary fake controller binaries still execute and fail closed.
#[cfg(any(test, feature = "test-support"))]
fn test_controller_identity(path: &Path, verified_digest: Option<&str>) -> Option<Result<String>> {
    let source = std::env::var_os(TEST_CONTROLLER_RUNTIME_EXECUTABLE_ENV)?;
    let identity = std::env::var(TEST_CONTROLLER_RUNTIME_IDENTITY_ENV).ok()?;
    crate::test_support::ensure_test_controller_fixture(Path::new(&source));
    let source_digest = test_controller_fixture_digest(Path::new(&source)).map_err(|error| {
        Error::validation_invalid_argument(
            "controller_runtime",
            format!("test controller source cannot be hashed: {error}"),
            Some(PathBuf::from(&source).display().to_string()),
            None,
        )
    });
    let candidate_digest = match verified_digest {
        Some(digest) => Ok(digest.to_string()),
        None => test_candidate_or_executable_digest(path),
    };
    match (source_digest, candidate_digest) {
        (Ok(source_digest), Ok(candidate_digest)) if source_digest == candidate_digest => {
            Some(Ok(identity))
        }
        (Err(error), _) | (_, Err(error)) => Some(Err(error)),
        _ => None,
    }
}

#[cfg(all(unix, any(test, feature = "test-support")))]
fn test_controller_fixture_digest(path: &Path) -> Result<String> {
    let file_identity = executable_file_identity(path)?;
    let cache = TEST_CONTROLLER_FIXTURE_DIGESTS.get_or_init(|| Mutex::new(BTreeMap::new()));
    if let Some(digest) = cache
        .lock()
        .expect("test controller source digest cache is not poisoned")
        .get(&file_identity)
        .cloned()
    {
        return Ok(digest);
    }

    #[cfg(test)]
    TEST_CONTROLLER_FIXTURE_DIGEST_CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let digest = executable_digest(path)?;
    cache
        .lock()
        .expect("test controller source digest cache is not poisoned")
        .insert(file_identity, digest.clone());
    Ok(digest)
}

#[cfg(all(not(unix), any(test, feature = "test-support")))]
fn test_controller_fixture_digest(path: &Path) -> Result<String> {
    executable_digest(path)
}

#[cfg(all(unix, any(test, feature = "test-support")))]
fn test_registered_fixture_digest(path: &Path) -> Option<String> {
    let file_identity = executable_file_identity(path).ok()?;
    TEST_CONTROLLER_FIXTURE_DIGESTS
        .get_or_init(|| Mutex::new(BTreeMap::new()))
        .lock()
        .expect("test controller fixture digest cache is not poisoned")
        .get(&file_identity)
        .cloned()
}

#[cfg(all(unix, any(test, feature = "test-support")))]
fn test_candidate_or_executable_digest(path: &Path) -> Result<String> {
    test_registered_fixture_digest(path).map_or_else(
        || {
            #[cfg(test)]
            TEST_CONTROLLER_FIXTURE_DIGEST_CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            executable_digest(path)
        },
        Ok,
    )
}

#[cfg(all(not(unix), any(test, feature = "test-support")))]
fn test_candidate_or_executable_digest(path: &Path) -> Result<String> {
    executable_digest(path)
}

#[cfg(not(any(test, feature = "test-support")))]
fn test_candidate_or_executable_digest(path: &Path) -> Result<String> {
    executable_digest(path)
}

#[cfg(all(unix, any(test, feature = "test-support")))]
fn controller_executable_digest(path: &Path) -> Result<String> {
    if std::env::var_os(TEST_CONTROLLER_RUNTIME_EXECUTABLE_ENV)
        .is_some_and(|source| Path::new(&source) == path)
    {
        crate::test_support::ensure_test_controller_fixture(path);
        return test_controller_fixture_digest(path);
    }
    executable_digest(path)
}

#[cfg(not(all(unix, any(test, feature = "test-support"))))]
fn controller_executable_digest(path: &Path) -> Result<String> {
    executable_digest(path)
}

fn activated_executable_identity(path: &Path) -> Result<String> {
    executable_identity(path, None)
}

fn executable_digest(path: &Path) -> Result<String> {
    let identity = observed_executable_identity(path);
    if let Some(digest) = memoized_executable_digest(identity.as_ref()) {
        return Ok(digest);
    }
    // Stream the file. A controller binary is hundreds of megabytes in an
    // unoptimized build and `fs::read` would hold all of it resident purely to
    // hash it, in a process that is about to fork a controller runtime.
    #[cfg(all(test, unix))]
    EXECUTABLE_DIGEST_COMPUTATIONS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let hashing_started = std::time::Instant::now();
    let digest = content_hash::sha256_file(path).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("hash pinned controller executable".to_string()),
        )
    })?;
    if hashing_started.elapsed() >= digest_memo_min_hash_time() {
        memoize_executable_digest(identity, &digest);
    }
    Ok(digest)
}

/// Stat `path` into its observed identity. The one place the eight identity
/// fields are read, for both the digest memo and the fixture cache.
#[cfg(unix)]
fn executable_file_identity(path: &Path) -> Result<ExecutableFileIdentity> {
    use std::os::unix::fs::MetadataExt;

    let metadata = fs::metadata(path).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("inspect controller executable".to_string()),
        )
    })?;
    Ok(ExecutableFileIdentity {
        path: path.to_path_buf(),
        device: metadata.dev(),
        inode: metadata.ino(),
        size: metadata.size(),
        modified_seconds: metadata.mtime(),
        modified_nanoseconds: metadata.mtime_nsec(),
        changed_seconds: metadata.ctime(),
        changed_nanoseconds: metadata.ctime_nsec(),
    })
}

/// The memo's view: an unreadable path is simply not memoizable, never an error.
#[cfg(unix)]
fn observed_executable_identity(path: &Path) -> Option<ExecutableFileIdentity> {
    executable_file_identity(path).ok()
}

#[cfg(not(unix))]
fn observed_executable_identity(_path: &Path) -> Option<()> {
    None
}

#[cfg(unix)]
fn memoized_executable_digest(identity: Option<&ExecutableFileIdentity>) -> Option<String> {
    EXECUTABLE_DIGESTS
        .get_or_init(|| Mutex::new(BTreeMap::new()))
        .lock()
        .expect("controller executable digest memo is not poisoned")
        .get(identity?)
        .cloned()
}

#[cfg(not(unix))]
fn memoized_executable_digest(_identity: Option<&()>) -> Option<String> {
    None
}

#[cfg(unix)]
fn memoize_executable_digest(identity: Option<ExecutableFileIdentity>, digest: &str) {
    let Some(identity) = identity else {
        return;
    };
    EXECUTABLE_DIGESTS
        .get_or_init(|| Mutex::new(BTreeMap::new()))
        .lock()
        .expect("controller executable digest memo is not poisoned")
        .insert(identity, digest.to_string());
}

#[cfg(not(unix))]
fn memoize_executable_digest(_identity: Option<()>, _digest: &str) {}

/// Record a pin whose bytes this process just verified.
///
/// `pin_executable` validates the pin immediately after publishing it, which
/// would otherwise re-read the file it has just hashed. Observe the destination
/// once publication has finished mutating it -- sealing its mode and linking it
/// both move the inode's change time -- so the validation that follows resolves
/// from the memo instead of from disk.
fn memoize_published_pin(destination: &Path, digest: &str) {
    memoize_executable_digest(observed_executable_identity(destination), digest);
}

fn make_executable_read_only(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o500)).map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("seal controller runtime pin".to_string()),
            )
        })?;
    }
    Ok(())
}

fn is_executable(metadata: &fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        true
    }
}

fn pinned_path(identity: &str, digest: &str) -> Result<PathBuf> {
    Ok(controller_runtime_store_root()?
        .join("controller-runtimes")
        .join(format!(
            "{}-{}",
            paths::sanitize_path_segment(identity),
            digest
        ))
        .join("homeboy"))
}

fn recovered_pinned_path(identity: &str, digest: &str) -> Result<PathBuf> {
    Ok(controller_runtime_store_root()?
        .join("controller-runtimes")
        .join(format!(
            "{}-{}",
            paths::sanitize_path_segment(identity),
            digest
        ))
        .join(format!("recovery-{}", uuid::Uuid::new_v4()))
        .join("homeboy"))
}

fn controller_runtime_store_root() -> Result<PathBuf> {
    #[cfg(any(test, feature = "test-support"))]
    if let Some(path) = std::env::var_os(TEST_CONTROLLER_RUNTIME_STORE_ENV) {
        return Ok(PathBuf::from(path));
    }

    paths::homeboy_data()
}

fn publish_pin(source: &Path, destination: &Path, expected_digest: &str) -> Result<()> {
    if destination.exists() {
        let actual = executable_digest(destination)?;
        if actual == expected_digest {
            register_test_fixture_candidate(source, destination, expected_digest);
            memoize_published_pin(destination, expected_digest);
            return Ok(());
        }
        return Err(Error::validation_invalid_argument(
            "controller_runtime",
            format!(
                "immutable controller runtime path already contains different bytes: {}",
                destination.display()
            ),
            Some(destination.display().to_string()),
            None,
        ));
    }
    let parent = destination.parent().expect("pinned runtime has parent");
    fs::create_dir_all(parent).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("create controller runtime pin".to_string()),
        )
    })?;
    let staging = parent.join(format!(
        ".homeboy-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    fs::copy(source, &staging).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("stage controller runtime pin".to_string()),
        )
    })?;
    let actual = executable_digest(&staging)?;
    if actual != expected_digest {
        let _ = fs::remove_file(&staging);
        return Err(Error::validation_invalid_argument(
            "controller_runtime",
            format!(
                "controller runtime source hash mismatch while publishing: expected {expected_digest}, found {actual}"
            ),
            Some(source.display().to_string()),
            None,
        ));
    }
    make_executable_read_only(&staging)?;
    match fs::hard_link(&staging, destination) {
        Ok(()) => {
            let _ = fs::remove_file(&staging);
            register_test_fixture_candidate(source, destination, expected_digest);
            memoize_published_pin(destination, expected_digest);
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let _ = fs::remove_file(&staging);
            let actual = executable_digest(destination)?;
            if actual == expected_digest {
                register_test_fixture_candidate(source, destination, expected_digest);
                memoize_published_pin(destination, expected_digest);
                Ok(())
            } else {
                Err(Error::validation_invalid_argument(
                    "controller_runtime",
                    format!(
                        "immutable controller runtime path already contains different bytes: {}",
                        destination.display()
                    ),
                    Some(destination.display().to_string()),
                    None,
                ))
            }
        }
        Err(error) => {
            let _ = fs::remove_file(&staging);
            Err(Error::internal_io(
                error.to_string(),
                Some("publish controller runtime pin".to_string()),
            ))
        }
    }
}

#[cfg(all(unix, any(test, feature = "test-support")))]
fn register_test_fixture_candidate(source: &Path, candidate: &Path, expected_digest: &str) {
    let Some(configured_source) = std::env::var_os(TEST_CONTROLLER_RUNTIME_EXECUTABLE_ENV) else {
        return;
    };
    if Path::new(&configured_source) != source
        || test_registered_fixture_digest(source).as_deref() != Some(expected_digest)
    {
        return;
    }
    let Ok(candidate_identity) = executable_file_identity(candidate) else {
        return;
    };
    TEST_CONTROLLER_FIXTURE_DIGESTS
        .get_or_init(|| Mutex::new(BTreeMap::new()))
        .lock()
        .expect("test controller fixture digest cache is not poisoned")
        .insert(candidate_identity, expected_digest.to_string());
}

#[cfg(not(all(unix, any(test, feature = "test-support"))))]
fn register_test_fixture_candidate(_source: &Path, _candidate: &Path, _expected_digest: &str) {}

/// Resolve the test-support controller fixture's digest once, up front.
///
/// Pin validation consults that fixture, and its digest cache is process-wide
/// and deliberately not cleared between tests. A test that measures digest
/// computations therefore has to decide whether the fixture is warm rather than
/// inherit the answer from whichever tests ran before it — otherwise it passes
/// under `cargo test` and fails under nextest, which gives every test its own
/// process (#12226).
///
/// Best effort: with no fixture configured there is nothing to warm, and the
/// caller's measurement is unaffected.
#[cfg(all(unix, test))]
fn warm_test_controller_fixture_digest() {
    let Some(source) = std::env::var_os(TEST_CONTROLLER_RUNTIME_EXECUTABLE_ENV) else {
        return;
    };
    let source = PathBuf::from(source);
    crate::test_support::ensure_test_controller_fixture(&source);
    let _ = test_controller_fixture_digest(&source);
}

fn required_runtime_string<'a>(runtime: &'a Value, pointer: &str, label: &str) -> Result<&'a str> {
    runtime
        .pointer(pointer)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "controller_runtime",
                format!("controller runtime pin has no {label}"),
                None,
                None,
            )
        })
}

fn build_source_provenance(identity: &build_identity::BuildIdentity) -> Value {
    let Some(revision) = identity.git_commit.as_deref() else {
        return unavailable_source_provenance();
    };
    json!({
        "repository": env!("CARGO_PKG_REPOSITORY"),
        "revision": revision,
        "verification": "build_metadata",
    })
}

fn unavailable_source_provenance() -> Value {
    json!({
        "state": "unavailable",
        "reason": "executable_build_or_install_provenance_not_recorded",
    })
}

fn run_command<const N: usize>(program: &str, args: [&str; N]) -> Result<()> {
    let status = Command::new(program)
        .args(args)
        .status()
        .map_err(|error| Error::internal_io(error.to_string(), Some(format!("run {program}"))))?;
    if status.success() {
        Ok(())
    } else {
        Err(Error::validation_invalid_argument(
            "controller_runtime",
            format!("{program} command failed during runtime recovery"),
            None,
            None,
        ))
    }
}

#[cfg(test)]
mod identity_probe_tests {
    use super::*;

    /// The running executable under `cargo test` is a libtest binary, which
    /// would treat `self identity` as test-name filters and re-run the suite.
    /// Resolving our own identity must never depend on spawning anything.
    #[test]
    fn the_current_executable_reports_its_compiled_identity_without_spawning() {
        let current = std::env::current_exe().expect("current test executable");

        assert!(is_current_executable(&current));
        assert_eq!(
            executable_identity(&current, None).expect("self identity"),
            build_identity::current().display
        );
    }

    /// A controller-runtime pin is a *copy* under a content-addressed
    /// directory, so it is never the same path as the executable it came from.
    /// Identity must still resolve without executing it.
    #[cfg(unix)]
    #[test]
    fn a_byte_identical_copy_resolves_without_spawning() {
        let temp = tempfile::tempdir().expect("tempdir");
        let current = std::env::current_exe().expect("current test executable");
        let copy = temp.path().join("homeboy");
        fs::copy(&current, &copy).expect("copy the running executable");

        assert!(!is_current_executable(&copy), "the copy is a distinct path");
        assert!(is_current_executable_content(&copy, None));
        assert_eq!(
            executable_identity(&copy, None).expect("copy identity"),
            build_identity::current().display
        );
    }

    /// The digest the caller already verified is authoritative: a candidate
    /// carrying some other binary's digest must not be mistaken for us.
    #[test]
    fn a_foreign_verified_digest_is_not_treated_as_self() {
        let current = std::env::current_exe().expect("current test executable");

        assert!(!is_current_executable_content(
            Path::new("/definitely/missing/homeboy"),
            Some("0000000000000000000000000000000000000000000000000000000000000000"),
        ));
        // ...while our own digest still identifies us through that same seam.
        let digest = executable_digest(&current).expect("digest the running executable");
        assert!(is_current_executable_content(
            Path::new("/definitely/missing/homeboy"),
            Some(&digest)
        ));
    }

    #[test]
    fn a_path_that_is_not_the_running_executable_is_not_treated_as_self() {
        let temp = tempfile::tempdir().expect("tempdir");
        let other = temp.path().join("not-the-current-exe");
        fs::write(&other, b"not an executable").expect("write candidate");

        assert!(!is_current_executable(&other));
        // A path that cannot be resolved at all must also not claim to be us.
        assert!(!is_current_executable(Path::new(
            "/definitely/missing/homeboy"
        )));
    }

    #[test]
    fn probe_output_within_the_cap_is_carried_verbatim() {
        assert_eq!(bounded_identity_output("boom"), "boom");
    }

    #[test]
    fn oversized_probe_output_is_truncated_and_reports_its_true_size() {
        let noisy = "x".repeat(4096);
        let bounded = bounded_identity_output(&noisy);

        assert!(bounded.len() < noisy.len());
        assert!(bounded.starts_with("xxxx"));
        assert!(
            bounded.contains("[truncated, 4096 bytes total]"),
            "{bounded}"
        );
    }

    /// Truncation slices bytes, so it must not split a multi-byte character.
    #[test]
    fn truncation_never_splits_a_multibyte_character() {
        let noisy = "é".repeat(4096);
        let bounded = bounded_identity_output(&noisy);

        assert!(bounded.contains("[truncated,"), "{bounded}");
        assert!(bounded.starts_with('é'));
    }
}

#[cfg(test)]
mod tests {

    /// A test is the entry point for its own unit of work, so the runtime root
    /// resolves once here and the rooted entry points take it (#7505).
    fn test_runtime_root() -> std::path::PathBuf {
        runtime_root_in(
            &crate::paths::PathRoots::from_environment()
                .expect("path roots")
                .data()
                .to_path_buf(),
        )
        .expect("runtime root")
    }
    use super::*;

    #[cfg(unix)]
    fn fake_controller(path: &Path, identity: &str, marker: &str) -> String {
        use std::os::unix::fs::PermissionsExt;

        let identity = serde_json::to_string(identity).expect("serialize fake identity");
        fs::write(
            path,
            format!(
                "#!/bin/sh\n# {marker}\nif [ \"$1\" = self ] && [ \"$2\" = identity ]; then\n  printf '%s\\n' '{{\"data\":{{\"display\":{identity}}}}}'\n  exit 0\nfi\nif [ \"$1\" = self ] && [ \"$2\" = status ]; then\n  printf '%s\\n' '{{\"data\":{{\"active_build_identity\":{{\"display\":{identity}}}}}}}'\n  exit 0\nfi\nexit 1\n"
            ),
        )
        .expect("write fake controller");
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .expect("make fake controller executable");
        executable_digest(path).expect("hash fake controller")
    }

    #[test]
    fn admission_lock_holder() {
        let Ok(path) = std::env::var("HOMEBOY_ADMISSION_LOCK_HELPER_PATH") else {
            return;
        };
        let ready = PathBuf::from(
            std::env::var("HOMEBOY_ADMISSION_LOCK_HELPER_READY").expect("helper ready path"),
        );
        let _guard = acquire_admission_lock_with_retry(Path::new(&path), 1, Duration::ZERO)
            .expect("helper admission guard");
        fs::write(&ready, b"ready").expect("signal helper readiness");
        if std::env::var_os("HOMEBOY_ADMISSION_LOCK_HELPER_EXIT").is_some() {
            std::process::exit(0);
        }
        let release = PathBuf::from(
            std::env::var("HOMEBOY_ADMISSION_LOCK_HELPER_RELEASE").expect("helper release path"),
        );
        for _ in 0..1_000 {
            if release.exists() {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("admission lock helper was not released");
    }

    fn spawn_admission_lock_holder(
        path: &Path,
        temporary: &Path,
        exit_without_drop: bool,
    ) -> std::process::Child {
        let ready = temporary.join("ready");
        let release = temporary.join("release");
        let mut command = Command::new(std::env::current_exe().expect("test executable"));
        command
            .args([
                "--exact",
                "controller_runtime::tests::admission_lock_holder",
                "--nocapture",
            ])
            .env("HOMEBOY_ADMISSION_LOCK_HELPER_PATH", path)
            .env("HOMEBOY_ADMISSION_LOCK_HELPER_READY", &ready)
            .env("HOMEBOY_ADMISSION_LOCK_HELPER_RELEASE", &release);
        if exit_without_drop {
            command.env("HOMEBOY_ADMISSION_LOCK_HELPER_EXIT", "1");
        }
        let child = command.spawn().expect("spawn admission lock holder");
        for _ in 0..500 {
            if ready.exists() {
                return child;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("admission lock holder did not become ready");
    }

    fn release_admission_lock_holder(mut child: std::process::Child, temporary: &Path) {
        fs::write(temporary.join("release"), b"release").expect("release admission lock holder");
        assert!(child
            .wait()
            .expect("wait for admission lock holder")
            .success());
    }

    #[test]
    fn live_admission_guard_cannot_be_stolen() {
        let temporary = tempfile::tempdir().expect("temporary runtime directory");
        let path = temporary.path().join(ADMISSION_LOCK_DIR);
        let child = spawn_admission_lock_holder(&path, temporary.path(), false);

        let attempt = acquire_admission_lock_with_retry(&path, 2, Duration::ZERO);
        release_admission_lock_holder(child, temporary.path());
        let error = attempt.expect_err("live admission guard must remain exclusive");

        assert!(error.message.contains("admission timed out"));
        assert!(error.message.contains("pid="));
        assert!(error.message.contains("waited"));
    }

    #[test]
    fn legacy_admission_lock_fails_closed() {
        let temporary = tempfile::tempdir().expect("temporary runtime directory");
        let path = temporary.path().join(ADMISSION_LOCK_DIR);
        fs::create_dir(&path).expect("create legacy lock directory");

        let error = acquire_admission_lock_with_retry(&path, 1, Duration::ZERO)
            .expect_err("legacy directory lock must not be stolen");

        assert!(error
            .message
            .contains("remove the abandoned directory explicitly"));
        assert!(path.is_dir());
    }

    #[test]
    fn admission_lock_is_released_when_holder_exits_without_drop() {
        let temporary = tempfile::tempdir().expect("temporary runtime directory");
        let path = temporary.path().join(ADMISSION_LOCK_DIR);
        let mut child = spawn_admission_lock_holder(&path, temporary.path(), true);

        assert!(child
            .wait()
            .expect("wait for exiting lock holder")
            .success());
        acquire_admission_lock_with_retry(&path, 1, Duration::ZERO)
            .expect("kernel releases lock after holder exits");
    }

    fn stale_admission_owner(pid: Option<u32>, starttime: Option<u64>, token: &str) -> Value {
        json!({
            "schema": ADMISSION_OWNER_SCHEMA,
            "token": token,
            "pid": pid,
            "linux_starttime_ticks": starttime,
        })
    }

    fn write_admission_owner(path: &Path, owner: &Value) {
        fs::write(
            path,
            serde_json::to_vec(owner).expect("serialize admission owner"),
        )
        .expect("write admission owner");
    }

    #[cfg(unix)]
    #[test]
    fn admission_reclaims_a_provably_dead_owner_with_a_new_fence() {
        let temporary = tempfile::tempdir().expect("temporary runtime directory");
        let path = temporary.path().join(ADMISSION_LOCK_DIR);
        let mut child = Command::new("sh")
            .args(["-c", "exit 0"])
            .spawn()
            .expect("spawn exiting process");
        let pid = child.id();
        child.wait().expect("wait for exiting process");
        let starttime = cfg!(target_os = "linux").then_some(1);
        write_admission_owner(&path, &stale_admission_owner(Some(pid), starttime, "dead"));

        let admission = acquire_admission_lock_with_retry(&path, 1, Duration::ZERO)
            .expect("reclaim dead owner");

        assert_ne!(admission.token, "dead");
        assert_eq!(admission_owner_token(&path), Some(admission.token.clone()));
    }

    #[test]
    fn admission_refuses_a_live_owner() {
        let temporary = tempfile::tempdir().expect("temporary runtime directory");
        let path = temporary.path().join(ADMISSION_LOCK_DIR);
        let owner = stale_admission_owner(Some(42), Some(1), "live");
        let error = ensure_admission_owner_is_recoverable_with(&owner, |_, _| {
            crate::process::ProcessIdentityState::Live
        })
        .expect_err("live owner must remain protected");
        assert!(error.message.contains("still live"));
        write_admission_owner(&path, &stale_admission_owner(None, None, "live"));

        let error = acquire_admission_lock_with_retry(&path, 1, Duration::ZERO)
            .expect("PID-less legacy record is recoverable");

        assert_ne!(error.token, "live");
    }

    #[test]
    fn admission_refuses_an_unverifiable_owner() {
        let owner = stale_admission_owner(Some(42), Some(1), "unknown");

        let error = ensure_admission_owner_is_recoverable_with(&owner, |_, _| {
            crate::process::ProcessIdentityState::Unverifiable
        })
        .expect_err("unverifiable owner must remain protected");

        assert!(error.message.contains("cannot be verified"));
    }

    #[test]
    fn admission_refuses_a_reused_pid_with_a_mismatched_identity() {
        let owner = stale_admission_owner(Some(42), Some(1), "reused");
        let error = ensure_admission_owner_is_recoverable_with(&owner, |_, _| {
            crate::process::ProcessIdentityState::IdentityMismatch
        })
        .expect_err("PID reuse must fail closed");

        assert!(error.message.contains("reused"));
    }

    #[test]
    fn admission_refuses_a_changed_owner_during_reclaim_compare_and_swap() {
        let temporary = tempfile::tempdir().expect("temporary runtime directory");
        let path = temporary.path().join(ADMISSION_LOCK_DIR);
        write_admission_owner(&path, &stale_admission_owner(None, None, "before"));
        *TEST_ADMISSION_OWNER_CAS_REPLACEMENT
            .get_or_init(|| Mutex::new(None))
            .lock()
            .expect("admission owner test hook is not poisoned") =
            Some(stale_admission_owner(None, None, "after"));

        let error = acquire_admission_lock_for(&path, "test-admission")
            .expect_err("changed owner must fail closed");

        assert!(error.message.contains("changed while reclaiming"));
        assert_eq!(admission_owner_token(&path), Some("after".to_string()));
    }

    #[test]
    fn admission_handles_legacy_pidless_and_malformed_owner_records_safely() {
        let temporary = tempfile::tempdir().expect("temporary runtime directory");
        let path = temporary.path().join(ADMISSION_LOCK_DIR);
        write_admission_owner(&path, &stale_admission_owner(None, None, "pidless"));
        let admission = acquire_admission_lock_with_retry(&path, 1, Duration::ZERO)
            .expect("PID-less legacy record is recoverable");
        drop(admission);

        write_admission_owner(&path, &json!({ "token": "malformed" }));
        let error = acquire_admission_lock_with_retry(&path, 1, Duration::ZERO)
            .expect_err("malformed record must fail closed");
        assert!(error.message.contains("malformed"));
        assert_eq!(admission_owner_token(&path), Some("malformed".to_string()));
    }

    #[test]
    fn admission_can_be_acquired_and_released_repeatedly() {
        let temporary = tempfile::tempdir().expect("temporary runtime directory");
        let path = temporary.path().join(ADMISSION_LOCK_DIR);

        for _ in 0..3 {
            let admission = acquire_admission_lock_with_retry(&path, 1, Duration::ZERO)
                .expect("acquire admission");
            drop(admission);
            assert!(fs::read(&path).expect("read released owner").is_empty());
        }
    }

    #[test]
    fn admission_guard_releases_after_post_acquisition_failure() {
        let temporary = tempfile::tempdir().expect("temporary runtime directory");
        let path = temporary.path().join(ADMISSION_LOCK_DIR);
        let result: Result<()> = (|| {
            let _guard = acquire_admission_lock_with_retry(&path, 1, Duration::ZERO)?;
            Err(Error::internal_unexpected("simulated pinning failure"))
        })();
        result.expect_err("simulated post-acquisition failure");

        acquire_admission_lock_with_retry(&path, 1, Duration::ZERO)
            .expect("next admission acquires released guard");
    }

    #[test]
    fn admission_timeout_reports_owner_and_wait_duration() {
        let temporary = tempfile::tempdir().expect("temporary runtime directory");
        let path = temporary.path().join(ADMISSION_LOCK_DIR);
        let child = spawn_admission_lock_holder(&path, temporary.path(), false);

        let attempt = acquire_admission_lock_with_retry(&path, 3, Duration::from_millis(1));
        release_admission_lock_holder(child, temporary.path());
        let error = attempt.expect_err("second admission times out");

        assert!(error.message.contains("waited"));
        assert!(error.message.contains("pid="));
        assert!(error.message.contains("token="));
    }

    /// #9373: an admission diagnostic that cannot name its holder is
    /// unactionable by construction. No resolution path may print the old
    /// `unavailable` placeholder.
    #[test]
    fn admission_owner_summary_never_reports_unavailable() {
        let temporary = tempfile::tempdir().expect("temporary runtime directory");
        let path = temporary.path().join(ADMISSION_LOCK_DIR);

        let absent = admission_owner_summary(&path);
        assert!(!absent.contains("unavailable"), "{absent}");
        assert!(absent.contains("no live owner"), "{absent}");
        assert!(absent.contains("retry"), "{absent}");

        // A released guard truncates the record; the lock is free, so the
        // summary must say retry rather than name a placeholder.
        fs::write(&path, b"").expect("truncate owner record");
        let released = admission_owner_summary(&path);
        assert!(!released.contains("unavailable"), "{released}");
        assert!(released.contains("retry"), "{released}");

        // An unreadable record still resolves to observable lock state.
        fs::write(&path, b"{ not json").expect("write malformed owner record");
        let malformed = admission_owner_summary(&path);
        assert!(!malformed.contains("unavailable"), "{malformed}");
        assert!(malformed.contains("retry"), "{malformed}");

        // A record without a PID names the fence and its reclaim consequence
        // instead of dropping to a placeholder.
        write_admission_owner(&path, &stale_admission_owner(None, None, "pidless"));
        let pidless = admission_owner_summary(&path);
        assert!(!pidless.contains("unavailable"), "{pidless}");
        assert!(pidless.contains("token=pidless"), "{pidless}");
        assert!(pidless.contains("reclaims"), "{pidless}");

        // A live owner is named with a verified liveness verdict.
        write_admission_owner(&path, &admission_owner_record("live-token"));
        let live = admission_owner_summary(&path);
        assert!(!live.contains("unavailable"), "{live}");
        assert!(
            live.contains(&format!("pid={}", std::process::id())),
            "{live}"
        );
        assert!(live.contains("token=live-token"), "{live}");
        assert!(live.contains("(live;"), "{live}");
    }

    /// The durable queue outlives the owning process, so it can name a holder
    /// even while the owner record is being rewritten between claim and
    /// publication (#9373).
    #[test]
    fn admission_owner_summary_falls_back_to_the_durable_queue_owner() {
        let temporary = tempfile::tempdir().expect("temporary runtime directory");
        let path = temporary.path().join(ADMISSION_LOCK_DIR);
        update_admission_queue(&path, |queue| {
            queue["owner"] = json!({
                "request_id": "agent-task-queued",
                "pid": 4_294_967_294u64,
                "linux_starttime_ticks": null,
                "advisory_lock": true,
            });
        })
        .expect("persist durable queue owner");

        let summary = admission_owner_summary(&path);

        assert!(!summary.contains("unavailable"), "{summary}");
        assert!(summary.contains("agent-task-queued"), "{summary}");
        assert!(summary.contains("pid=4294967294"), "{summary}");
    }

    #[test]
    fn rooted_admission_cancellation_does_not_remove_the_same_id_from_another_root() {
        let left = tempfile::tempdir().expect("left runtime root");
        let right = tempfile::tempdir().expect("right runtime root");
        let request_id = "same-run-id";
        for root in [left.path(), right.path()] {
            enqueue_admission_request(&root.join(ADMISSION_LOCK_DIR), request_id)
                .expect("enqueue isolated waiter");
        }

        cancel_admission_at(left.path(), request_id).expect("cancel left waiter");

        assert!(read_admission_queue(&left.path().join(ADMISSION_LOCK_DIR))
            .expect("read left queue")["requests"]
            .as_array()
            .expect("left requests")
            .is_empty());
        assert_eq!(
            read_admission_queue(&right.path().join(ADMISSION_LOCK_DIR)).expect("read right queue")
                ["requests"][0]["request_id"],
            request_id
        );
    }

    /// A contended admission failure must carry machine-readable evidence, not
    /// just a sentence: durable records and follow-up commands join on it.
    #[test]
    fn admission_busy_error_carries_named_owner_evidence() {
        let temporary = tempfile::tempdir().expect("temporary runtime directory");
        let path = temporary.path().join(ADMISSION_LOCK_DIR);
        write_admission_owner(&path, &admission_owner_record("busy-token"));

        let error = admission_busy_error(&path);

        assert_eq!(error.retryable, Some(true));
        assert!(!error.message.contains("unavailable"), "{}", error.message);
        assert!(error.message.contains("token=busy-token"));
        assert!(error.details["controller_admission"]["owner_summary"]
            .as_str()
            .is_some_and(|summary| summary.contains("token=busy-token")));
        assert!(error.details["controller_admission"]["queueing"]
            .as_str()
            .is_some_and(|queueing| queueing.contains("FIFO")));
    }

    /// Uncoordinated callers (cleanup, generation activation, pin recovery)
    /// carry no durable request ID and cannot join the FIFO queue. They must
    /// still queue for a bounded window instead of fast-failing the moment a
    /// parallel cook wave holds admission (#9373).
    #[test]
    fn uncoordinated_admission_queues_for_a_bounded_window_before_reporting_contention() {
        let temporary = tempfile::tempdir().expect("temporary runtime directory");
        let path = temporary.path().join(ADMISSION_LOCK_DIR);
        let child = spawn_admission_lock_holder(&path, temporary.path(), false);

        let started = std::time::Instant::now();
        let attempt = acquire_admission_lock_bounded(&path, Duration::from_millis(400));
        let waited = started.elapsed();
        release_admission_lock_holder(child, temporary.path());
        let error = attempt.expect_err("a live holder still wins the bounded wait");

        assert!(
            waited >= Duration::from_millis(400),
            "bounded admission must queue, waited {waited:?}"
        );
        assert_eq!(error.retryable, Some(true));
        assert!(error.message.contains("pid="), "{}", error.message);

        // Once the holder releases, the same bounded acquisition succeeds.
        let admission =
            acquire_admission_lock_bounded(&path, Duration::ZERO).expect("released lock is free");
        drop(admission);
    }

    #[test]
    fn admission_queue_serializes_waiters_and_recovers_cancelled_and_stale_requests() {
        crate::test_support::with_isolated_home(|_| {
            let root = runtime_root().expect("runtime root");
            let lock = root.join(ADMISSION_LOCK_DIR);
            let first = admit_current_for("first").expect("first admission");
            let (acquired, acquired_result) = std::sync::mpsc::channel();
            let (release, release_result) = std::sync::mpsc::channel();
            let waiter = std::thread::spawn(move || match admit_current_for("second") {
                Ok(admission) => {
                    acquired.send(Ok(())).expect("report second admission");
                    release_result.recv().expect("release second admission");
                    drop(admission);
                }
                Err(error) => acquired
                    .send(Err(error.message))
                    .expect("report failed second admission"),
            });

            let waiting = (0..40)
                .map(|_| {
                    let status = admission_status("second").expect("second status");
                    if status["state"] == "waiting" {
                        Some(status)
                    } else {
                        std::thread::sleep(Duration::from_millis(25));
                        None
                    }
                })
                .find_map(|status| status)
                .expect("second admission waits behind first");
            assert_eq!(waiting["position"], 2);
            assert_eq!(
                admission_status("first").expect("first status")["state"],
                "admitted"
            );

            drop(first);
            assert_eq!(
                acquired_result
                    .recv_timeout(Duration::from_secs(30))
                    .expect("second admission resolves"),
                Ok(())
            );
            assert_eq!(
                admission_status("second").expect("second admitted")["state"],
                "admitted"
            );
            release.send(()).expect("release second admission");
            waiter.join().expect("waiter exits");

            enqueue_admission_request(&lock, "cancelled").expect("enqueue cancellation target");
            cancel_admission("cancelled").expect("cancel waiting request");
            assert_eq!(
                admission_status("cancelled").expect("cancelled status")["state"],
                "none"
            );

            enqueue_admission_request(&lock, "stale-waiter").expect("enqueue stale waiter");
            update_admission_queue(&lock, |queue| {
                queue["requests"] = json!([{
                    "request_id": "stale-waiter",
                    "state": "waiting",
                    "lease_expires_at_ms": 0,
                }]);
            })
            .expect("persist stale waiter");
            assert_eq!(
                admission_status("stale-waiter").expect("reclaim stale waiter")["state"],
                "none"
            );
        });
    }

    #[test]
    fn admission_status_retains_a_live_owner_after_its_lease_expires() {
        crate::test_support::with_isolated_home(|_| {
            let root = runtime_root().expect("runtime root");
            let lock = root.join(ADMISSION_LOCK_DIR);
            let admission = admit_current_for("long-owner").expect("admit owner");

            update_admission_queue(&lock, |queue| {
                queue["owner"]["lease_expires_at_ms"] = json!(0);
            })
            .expect("expire diagnostic lease");
            let status = admission_status("long-owner").expect("read live owner status");

            assert_eq!(status["state"], "admitted");
            assert_eq!(status["owner"]["request_id"], "long-owner");
            assert_eq!(
                status["owner"]["controller_generation"],
                build_identity::current().display
            );
            assert!(status["owner"]["runtime"].is_object());
            drop(admission);
        });
    }

    #[test]
    fn expired_head_waiter_is_reclaimed_without_blocking_later_admission() {
        crate::test_support::with_isolated_home(|_| {
            let root = runtime_root().expect("runtime root");
            let lock = root.join(ADMISSION_LOCK_DIR);
            update_admission_queue(&lock, |queue| {
                queue["requests"] = json!([
                    {
                        "request_id": "crashed-head",
                        "state": "waiting",
                        "requested_at_ms": 1,
                        "heartbeat_at_ms": 1,
                        "lease_expires_at_ms": 0,
                    },
                    {
                        "request_id": "later",
                        "state": "waiting",
                        "requested_at_ms": 2,
                        "heartbeat_at_ms": 2,
                        "lease_expires_at_ms": now_millis() + admission_queue_lease().as_millis() as u64,
                    }
                ]);
            })
            .expect("persist crashed queue head");

            assert_eq!(
                admission_status("later").expect("reclaim crashed head")["position"],
                1
            );
            let admission = acquire_queued_admission_lock(&lock, "later", &|| Ok(false))
                .expect("later waiter acquires after reclamation");
            drop(admission);
        });
    }

    #[test]
    fn bounded_queue_timeout_is_retryable_and_removes_its_waiter() {
        crate::test_support::with_isolated_home(|_| {
            let root = runtime_root().expect("runtime root");
            let lock = root.join(ADMISSION_LOCK_DIR);
            let first = admit_current_for("first").expect("admit first owner");
            enqueue_admission_request(&lock, "timed-out").expect("enqueue waiting request");

            let error = acquire_queued_admission_lock_with_timeout(
                &lock,
                "timed-out",
                Duration::ZERO,
                &|| Ok(false),
            )
            .expect_err("zero timeout expires deterministically");

            assert_eq!(error.retryable, Some(true));
            assert!(error.message.contains("queue wait exceeded 0ms"));
            assert_eq!(
                admission_status("timed-out").expect("timed-out request removed")["state"],
                "none"
            );
            drop(first);
        });
    }

    /// A timeout that returns `retryable: true` with nobody retrying it is the
    /// silent partial failure in a fanned-out wave. The error has to say what
    /// happened and what to run next, or 11 of 12 cooks vanish quietly.
    #[test]
    fn queue_timeout_names_the_wait_the_waiters_ahead_and_the_retry_command() {
        crate::test_support::with_isolated_home(|_| {
            let root = runtime_root().expect("runtime root");
            let lock = root.join(ADMISSION_LOCK_DIR);
            let first = admit_current_for("first").expect("admit first owner");
            enqueue_admission_request(&lock, "queued-behind").expect("enqueue waiting request");

            let error = acquire_queued_admission_lock_with_timeout(
                &lock,
                "queued-behind",
                Duration::ZERO,
                &|| Ok(false),
            )
            .expect_err("zero timeout expires deterministically");

            // The wait is reported, along with how many requests were ahead.
            // Not asserted as an exact figure: the elapsed wait is real wall
            // clock, so pinning it would be a timing flake.
            assert!(error.message.contains(" waited "), "{}", error.message);
            assert!(
                error.details["waited_ms"].is_u64(),
                "{}",
                error.details["waited_ms"]
            );
            assert_eq!(error.details["waiters_ahead"], 1);
            assert_eq!(error.details["queue_depth"], 2);
            assert_eq!(error.details["wait_timeout_ms"], 0);
            assert_eq!(error.details["request_id"], "queued-behind");

            // The failure is explicitly not self-healing, and names its retry.
            assert_eq!(error.details["automatic_retry"], false);
            assert_eq!(
                error.details["retry_command"],
                "homeboy agent-task retry queued-behind --run"
            );
            let hints = error
                .hints
                .iter()
                .map(|hint| hint.message.clone())
                .collect::<Vec<_>>()
                .join("\n");
            assert!(
                hints.contains("homeboy agent-task retry queued-behind --run"),
                "{hints}"
            );
            assert!(
                hints.contains("/controller_admission/queue_wait_timeout_ms"),
                "{hints}"
            );

            // The owner is still named, and the waiter is gone from the queue.
            assert!(
                error.message.contains("current owner:"),
                "{}",
                error.message
            );
            assert_eq!(
                admission_status("queued-behind").expect("waiter removed")["state"],
                "none"
            );
            drop(first);
        });
    }

    /// The backoff exists to cut poll cost for waiters that cannot be admitted
    /// yet. It must never grow past the heartbeat, because a waiter that sleeps
    /// through its own lease renewal is reclaimed as crashed — which would lose
    /// the FIFO slot the queue exists to protect.
    #[test]
    fn queue_backoff_grows_but_never_outlives_the_heartbeat() {
        let timings =
            AdmissionTimings::sanitized(&crate::defaults::ControllerAdmissionConfig::default());

        let mut interval = timings.poll;
        let mut observed = vec![interval];
        for _ in 0..12 {
            interval = next_admission_backoff(interval, &timings);
            observed.push(interval);
        }

        assert_eq!(observed[0], Duration::from_millis(250));
        assert_eq!(observed[1], Duration::from_millis(500));
        assert_eq!(observed[2], Duration::from_millis(1_000));
        assert_eq!(observed[3], Duration::from_millis(2_000));
        // Saturated at the ceiling, not growing without bound.
        assert!(observed
            .iter()
            .all(|interval| *interval <= timings.poll_max));
        assert_eq!(*observed.last().expect("intervals"), timings.poll_max);
        assert!(
            timings.poll_max <= timings.heartbeat,
            "backoff ceiling {:?} must not outlive the heartbeat {:?}",
            timings.poll_max,
            timings.heartbeat
        );

        // Even an operator asking for an absurd ceiling cannot break that.
        let greedy = AdmissionTimings::sanitized(&crate::defaults::ControllerAdmissionConfig {
            queue_poll_max_ms: 10 * 60 * 1_000,
            ..crate::defaults::ControllerAdmissionConfig::default()
        });
        assert!(greedy.poll_max <= greedy.heartbeat);
        assert_eq!(
            next_admission_backoff(greedy.heartbeat, &greedy),
            greedy.poll_max
        );
    }

    /// The wait strategy changed; the fairness contract did not. Ownership is
    /// gated on being the head of the durable queue, re-checked under the queue
    /// lock, so no amount of backoff or wakeup ordering lets a later request
    /// barge ahead of an earlier one.
    #[test]
    fn only_the_head_of_the_queue_may_claim_ownership() {
        crate::test_support::with_isolated_home(|_| {
            let root = runtime_root().expect("runtime root");
            let lock = root.join(ADMISSION_LOCK_DIR);
            for request_id in ["head", "middle", "tail"] {
                enqueue_admission_request(&lock, request_id).expect("enqueue request");
            }

            assert_eq!(
                admission_status("head").expect("head status")["position"],
                1
            );
            assert_eq!(
                admission_status("middle").expect("middle status")["position"],
                2
            );
            assert_eq!(
                admission_status("tail").expect("tail status")["position"],
                3
            );

            // Neither trailing request can take ownership while `head` waits,
            // however eagerly it polls.
            for barging in ["middle", "tail"] {
                assert!(
                    !claim_admission_owner(&lock, barging, "barging-token", &|| Ok(false))
                        .expect("claim attempt resolves"),
                    "{barging} must not claim admission ahead of the queue head"
                );
                assert!(
                    admission_status(barging).expect("queue unchanged")["owner"].is_null(),
                    "a rejected claim must not publish an owner"
                );
            }

            assert!(
                claim_admission_owner(&lock, "head", "head-token", &|| Ok(false))
                    .expect("head claim resolves"),
                "the queue head must be admitted"
            );
            assert_eq!(
                admission_status("head").expect("head admitted")["state"],
                "admitted"
            );
        });
    }

    #[test]
    fn admission_durations_render_in_operator_units() {
        assert_eq!(format_admission_duration(Duration::ZERO), "0ms");
        assert_eq!(
            format_admission_duration(Duration::from_millis(250)),
            "250ms"
        );
        assert_eq!(format_admission_duration(Duration::from_secs(30)), "30s");
        assert_eq!(
            format_admission_duration(Duration::from_secs(10 * 60)),
            "10m0s"
        );
        assert_eq!(format_admission_duration(Duration::from_secs(605)), "10m5s");
    }

    /// Config must reproduce the historical hardcoded constants exactly, so
    /// making these tunable cannot move behaviour out of the box.
    #[test]
    fn admission_timings_default_to_the_historical_constants() {
        let timings =
            AdmissionTimings::sanitized(&crate::defaults::ControllerAdmissionConfig::default());

        assert_eq!(timings.poll, Duration::from_millis(250));
        assert_eq!(timings.lease, Duration::from_secs(30));
        assert_eq!(timings.heartbeat, Duration::from_secs(5));
        assert_eq!(timings.wait_timeout, Duration::from_secs(10 * 60));
        assert_eq!(timings.busy_wait, Duration::from_secs(30));
    }

    #[test]
    fn admission_timings_are_config_driven() {
        crate::test_support::with_isolated_home(|_| {
            crate::defaults::save_config(&crate::defaults::HomeboyConfig {
                controller_admission: crate::defaults::ControllerAdmissionConfig {
                    queue_poll_ms: 40,
                    queue_poll_max_ms: 600,
                    queue_lease_ms: 4_000,
                    queue_heartbeat_ms: 800,
                    queue_wait_timeout_ms: 90_000,
                    busy_wait_ms: 7_000,
                },
                ..crate::defaults::HomeboyConfig::default()
            })
            .expect("save admission config");

            let timings = AdmissionTimings::load();
            assert_eq!(timings.poll, Duration::from_millis(40));
            assert_eq!(timings.poll_max, Duration::from_millis(600));
            assert_eq!(timings.lease, Duration::from_millis(4_000));
            assert_eq!(timings.heartbeat, Duration::from_millis(800));
            assert_eq!(timings.wait_timeout, Duration::from_millis(90_000));
            assert_eq!(timings.busy_wait, Duration::from_millis(7_000));

            // The accessors used outside the wait loop read the same config.
            assert_eq!(admission_queue_lease(), Duration::from_millis(4_000));
            assert_eq!(
                admission_queue_wait_timeout(),
                Duration::from_millis(90_000)
            );
            assert_eq!(admission_busy_wait(), Duration::from_millis(7_000));
        });
    }

    /// A heartbeat that cannot outrun its own lease lets the queue reclaim live
    /// waiters, which is exactly how FIFO order is silently lost. Config is
    /// sanitized rather than rejected because this lock gates every cook.
    #[test]
    fn admission_timings_sanitize_config_that_would_strand_waiters() {
        let floor = Duration::from_millis(crate::defaults::MIN_ADMISSION_INTERVAL_MS);

        let stranding = AdmissionTimings::sanitized(&crate::defaults::ControllerAdmissionConfig {
            queue_poll_ms: 0,
            queue_lease_ms: 1_000,
            queue_heartbeat_ms: 5_000,
            ..crate::defaults::ControllerAdmissionConfig::default()
        });
        assert!(
            stranding.heartbeat <= stranding.lease / 2,
            "heartbeat {:?} must renew at least twice per lease {:?}",
            stranding.heartbeat,
            stranding.lease
        );
        assert!(stranding.poll >= floor);
        assert!(stranding.poll <= stranding.heartbeat);

        let zeroed = AdmissionTimings::sanitized(&crate::defaults::ControllerAdmissionConfig {
            queue_poll_ms: 0,
            queue_poll_max_ms: 0,
            queue_lease_ms: 0,
            queue_heartbeat_ms: 0,
            queue_wait_timeout_ms: 0,
            busy_wait_ms: 0,
        });
        assert!(zeroed.poll >= floor, "a zero poll would spin, not wait");
        assert!(zeroed.poll_max >= floor);
        assert!(zeroed.heartbeat >= floor);
        assert!(zeroed.lease >= floor);
        assert!(zeroed.poll <= zeroed.heartbeat);
    }

    #[test]
    fn cancellation_between_head_observation_and_claim_never_publishes_owner() {
        crate::test_support::with_isolated_home(|_| {
            let root = runtime_root().expect("runtime root");
            let lock = root.join(ADMISSION_LOCK_DIR);
            let first = admit_current_for("first").expect("admit first owner");
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
            *TEST_ADMISSION_HEAD_BARRIER
                .get_or_init(|| Mutex::new(None))
                .lock()
                .expect("install head hook") = Some(barrier.clone());
            let waiter =
                std::thread::spawn(|| admit_current_for("racing").map(|admission| drop(admission)));

            drop(first);
            barrier.wait();
            cancel_admission("racing").expect("remove cancelled waiter");
            barrier.wait();

            let error = match waiter.join().expect("waiter exits") {
                Err(error) => error,
                Ok(()) => panic!("cancelled waiter cannot acquire admission"),
            };
            assert_eq!(error.details["outcome"], "cancelled");
            assert_eq!(
                admission_status("racing").expect("cancelled request is absent")["state"],
                "none"
            );
            assert!(
                admission_status("racing").expect("cancelled request has no owner")["owner"]
                    .is_null()
            );
            *TEST_ADMISSION_HEAD_BARRIER
                .get_or_init(|| Mutex::new(None))
                .lock()
                .expect("remove head hook") = None;
            let next = admit_current_for("next").expect("queue remains usable after cancellation");
            drop(next);
            let _ = lock;
        });
    }

    #[test]
    fn cancellation_after_owner_publication_does_not_steal_live_lock() {
        crate::test_support::with_isolated_home(|_| {
            let admission = admit_current_for("owner").expect("admit owner");
            cancel_admission("owner").expect("cancel admitted request");

            let status = admission_status("owner").expect("owner remains observable");
            assert_eq!(status["state"], "admitted");
            assert_eq!(status["owner"]["request_id"], "owner");
            drop(admission);
        });
    }

    #[test]
    #[cfg(unix)]
    fn identity_mismatch_returns_pinned_runtime_recovery_command() {
        let temporary = tempfile::tempdir().expect("temporary runtime directory");
        let pinned = temporary.path().join("homeboy-origin");
        let digest = fake_controller(&pinned, "homeboy 1.0.0+origin", "origin");
        make_executable_read_only(&pinned).expect("seal executable");
        let metadata = json!({
            "controller_runtime": {
                "originating": {
                    "build_identity": "homeboy 1.0.0+origin",
                    "pinned_executable": pinned,
                    "sha256": digest,
                }
            }
        });

        let error = validate_for_mutation(&metadata, "homeboy 1.0.0+replacement")
            .expect_err("replacement runtime must not mutate the originating lifecycle");

        assert!(error.message.contains("homeboy 1.0.0+origin"));
        assert!(error.details["tried"][0]
            .as_str()
            .is_some_and(|command| command.contains("homeboy-origin")));
    }

    #[test]
    #[cfg(unix)]
    fn identity_mismatch_resolves_the_verified_pinned_executable() {
        let temporary = tempfile::tempdir().expect("temporary runtime directory");
        let pinned = temporary.path().join("homeboy-origin");
        let digest = fake_controller(&pinned, "homeboy 1.0.0+origin", "origin");
        make_executable_read_only(&pinned).expect("seal executable");
        let metadata = json!({
            "controller_runtime": {
                "originating": {
                    "build_identity": "homeboy 1.0.0+origin",
                    "pinned_executable": pinned,
                    "sha256": digest,
                }
            }
        });

        assert_eq!(
            pinned_executable_for_mutation(&metadata, "homeboy 1.0.0+replacement")
                .expect("verified pin")
                .as_deref(),
            Some(pinned.as_path())
        );
        assert!(
            pinned_executable_for_mutation(&metadata, "homeboy 1.0.0+origin")
                .expect("origin runtime")
                .is_none()
        );
    }

    #[test]
    fn altered_or_missing_pinned_executable_fails_closed() {
        let temporary = tempfile::tempdir().expect("temporary runtime directory");
        let pinned = temporary.path().join("homeboy");
        fs::write(&pinned, b"generation-a").expect("write pinned executable");
        make_executable_read_only(&pinned).expect("seal executable");
        let runtime = json!({
            "originating": {
                "pinned_executable": pinned,
                "sha256": executable_digest(&pinned).expect("hash executable")
            }
        });
        fs::remove_file(
            runtime
                .pointer("/originating/pinned_executable")
                .and_then(Value::as_str)
                .expect("path"),
        )
        .expect("remove pinned executable");
        assert!(validate_pin(&runtime).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn installed_generation_switch_publishes_b_and_retains_a_pin() {
        use std::os::unix::fs::PermissionsExt;

        crate::test_support::with_isolated_home(|_| {
            let temporary = tempfile::tempdir().expect("temporary executable directory");
            let generation_a = temporary.path().join("homeboy-a");
            let generation_b = temporary.path().join("homeboy-b");
            for (path, identity) in [
                (&generation_a, "homeboy 0.1.0+generation-a"),
                (&generation_b, "homeboy 0.1.0+generation-b"),
            ] {
                let identity = serde_json::to_string(identity).expect("serialize identity");
                fs::write(
                    path,
                    format!(
                        "#!/bin/sh\nif [ \"$1\" = self ] && [ \"$2\" = identity ]; then\n  printf '%s\\n' '{{\"data\":{{\"display\":{identity}}}}}'\n  exit 0\nfi\nexit 1\n"
                    ),
                )
                .expect("write generation executable");
                fs::set_permissions(path, fs::Permissions::from_mode(0o755))
                    .expect("make generation executable");
            }

            let runtime_a = activate_installed_generation(&generation_a)
                .expect("activate installed generation A");
            let runtime_b = activate_installed_generation(&generation_b)
                .expect("activate installed generation B");

            assert_eq!(
                runtime_a["originating"]["build_identity"],
                "homeboy 0.1.0+generation-a"
            );
            assert_eq!(
                runtime_b["originating"]["build_identity"],
                "homeboy 0.1.0+generation-b"
            );
            validate_pin(&runtime_a).expect("generation A pin remains valid");
            validate_pin(&runtime_b).expect("generation B pin is valid");

            let active: Value = serde_json::from_str(
                &fs::read_to_string(
                    runtime_root()
                        .expect("runtime root")
                        .join(ACTIVE_GENERATION_FILE),
                )
                .expect("read active generation"),
            )
            .expect("parse active generation");
            assert_eq!(
                active["originating"]["build_identity"],
                "homeboy 0.1.0+generation-b"
            );
        });
    }

    #[test]
    fn admission_replaces_a_stale_active_generation_with_the_submitting_runtime() {
        crate::test_support::with_isolated_home(|_| {
            let mut runtime_a = pin_current_in_root(&test_runtime_root()).expect("pin runtime A");
            runtime_a["originating"]["build_identity"] = json!("homeboy runtime-a");
            runtime_a["requested"] = json!("homeboy runtime-a");
            runtime_a["current"] = json!("homeboy runtime-a");
            runtime_a["executed"] = json!("homeboy runtime-a");
            let active = runtime_root()
                .expect("runtime root")
                .join(ACTIVE_GENERATION_FILE);
            write_active_generation(&active, &runtime_a).expect("write stale runtime A");

            let runtime_b = admit_current().expect("runtime B admission");
            let current = build_identity::current().display;

            assert_eq!(runtime_b.runtime["originating"]["build_identity"], current);
            assert_eq!(runtime_b.runtime["requested"], current);
            validate_for_mutation(
                &json!({ CONTROLLER_RUNTIME_METADATA_KEY: runtime_a }),
                &current,
            )
            .expect_err("runtime B must retain runtime A's immutable pin");
            validate_for_mutation(
                &json!({ CONTROLLER_RUNTIME_METADATA_KEY: runtime_b.runtime }),
                &current,
            )
            .expect("runtime B can mutate its fresh run");

            let active: Value = serde_json::from_str(
                &fs::read_to_string(active).expect("read refreshed active generation"),
            )
            .expect("parse refreshed active generation");
            assert_eq!(active["originating"]["build_identity"], current);
        });
    }

    #[test]
    fn pin_current_uses_the_explicit_test_controller_fixture() {
        crate::test_support::with_isolated_home(|_| {
            let runtime =
                pin_current_in_root(&test_runtime_root()).expect("pin explicit controller fixture");
            let source = runtime
                .pointer("/originating/executable")
                .and_then(Value::as_str)
                .map(PathBuf::from)
                .expect("controller fixture source");
            let pinned = runtime
                .pointer("/originating/pinned_executable")
                .and_then(Value::as_str)
                .map(PathBuf::from)
                .expect("pinned executable");

            assert_eq!(
                source,
                crate::test_support::controller_runtime_test_executable()
            );
            assert_ne!(
                source,
                std::env::current_exe().expect("current test executable")
            );
            assert_eq!(
                executable_identity(&pinned, None).expect("fixture identity"),
                build_identity::current().display
            );
        });
    }

    #[cfg(unix)]
    #[test]
    fn explicitly_pinned_executable_reports_unavailable_source_provenance() {
        crate::test_support::with_isolated_home(|_| {
            let temporary = tempfile::tempdir().expect("temporary executable directory");
            let executable = temporary.path().join("homeboy");
            let identity = "homeboy 0.0.0+external";
            let digest = fake_controller(&executable, identity, "external");

            let runtime = pin_executable(&executable, identity).expect("pin external executable");

            assert_eq!(runtime["originating"]["sha256"], digest);
            assert_eq!(runtime["originating"]["build_identity"], identity);
            assert_eq!(runtime["originating"]["source"]["state"], "unavailable");
            assert_eq!(
                runtime["originating"]["source"]["reason"],
                "executable_build_or_install_provenance_not_recorded"
            );
        });
    }

    #[cfg(unix)]
    /// Lower the digest memo's settle threshold for one test, restoring it on
    /// drop.
    ///
    /// Leaking this would re-enable memoization for every later test in the
    /// process, which is precisely the condition that let a stale digest be
    /// reused -- so it is restored on unwind, not at the end of a happy path.
    #[cfg(unix)]
    struct SettleThresholdOverride;

    #[cfg(unix)]
    impl SettleThresholdOverride {
        fn of_millis(millis: u64) -> Self {
            DIGEST_MEMO_MIN_HASH_TIME_MS.store(millis, std::sync::atomic::Ordering::Relaxed);
            Self
        }
    }

    #[cfg(unix)]
    impl Drop for SettleThresholdOverride {
        fn drop(&mut self) {
            DIGEST_MEMO_MIN_HASH_TIME_MS.store(u64::MAX, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// The regression test for the memo returning a digest for bytes that no
    /// longer exist.
    ///
    /// `ctime`/`mtime` come from a coarse clock that advances every 1ms here, so
    /// an in-place same-size rewrite issued in the same tick as the hash moves
    /// no observed identity field. Both writes below are twelve bytes, which is
    /// exactly the shape that slipped past `publication_is_no_clobber_and_idempotent`:
    /// a size change would have been caught, and an identical size was not.
    #[test]
    #[cfg(unix)]
    fn a_hash_faster_than_the_timestamp_clock_is_not_memoized() {
        crate::test_support::with_isolated_home(|_| {
            EXECUTABLE_DIGESTS
                .get_or_init(|| Mutex::new(BTreeMap::new()))
                .lock()
                .expect("controller executable digest memo is not poisoned")
                .clear();

            let temporary = tempfile::tempdir().expect("temporary executable directory");
            let executable = temporary.path().join("homeboy");

            fs::write(&executable, b"generation-a").expect("write first generation");
            let first = executable_digest(&executable).expect("hash first generation");

            // Same length, same inode, same tick: nothing the identity observes
            // has changed.
            fs::write(&executable, b"generation-b").expect("write second generation");
            let second = executable_digest(&executable).expect("hash second generation");

            assert_ne!(
                first, second,
                "a same-size rewrite inside one timestamp tick must not reuse the prior digest"
            );
        });
    }

    /// `chmod` `path` to `mode` and return once the change is visible in the
    /// file's observed identity.
    ///
    /// `ctime` comes from the kernel's coarse timestamp clock, which advances
    /// every 1ms on this ext4 host. A `chmod` issued in the same tick as the
    /// operation before it produces a byte-identical `ExecutableFileIdentity`,
    /// and the digest memo then hits -- correctly, because the bytes did not
    /// change.
    ///
    /// So a test asserting that a chmod is *observed* is asserting something
    /// about the clock, not about the cache, and it fails on any machine fast
    /// enough to fit both operations in one tick. Waiting for the clock states
    /// that precondition instead of racing it.
    #[cfg(unix)]
    fn chmod_until_observable(path: &Path, mode: u32) {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let before = fs::metadata(path).expect("stat candidate before chmod");
        let before = (before.ctime(), before.ctime_nsec());

        for _ in 0..5_000 {
            fs::set_permissions(path, fs::Permissions::from_mode(mode))
                .expect("change candidate mode");
            let after = fs::metadata(path).expect("stat candidate after chmod");
            if (after.ctime(), after.ctime_nsec()) != before {
                return;
            }
            std::thread::sleep(std::time::Duration::from_micros(200));
        }

        panic!("chmod never became observable in the candidate's change time");
    }

    #[test]
    fn test_fixture_identity_cache_reuses_sealed_candidates_and_invalidates_changes() {
        use std::os::unix::fs::PermissionsExt;

        crate::test_support::with_isolated_home(|_| {
            TEST_CONTROLLER_FIXTURE_DIGESTS
                .get_or_init(|| Mutex::new(BTreeMap::new()))
                .lock()
                .expect("test controller source digest cache is not poisoned")
                .clear();
            TEST_CONTROLLER_FIXTURE_DIGEST_CALLS.store(0, std::sync::atomic::Ordering::Relaxed);

            let runtime =
                pin_current_in_root(&test_runtime_root()).expect("pin test controller fixture");
            let candidate = runtime
                .pointer("/originating/pinned_executable")
                .and_then(Value::as_str)
                .map(PathBuf::from)
                .expect("pinned candidate");
            validate_pin(&runtime).expect("first fixture identity validation");
            validate_pin(&runtime).expect("second fixture identity validation");
            assert_eq!(
                TEST_CONTROLLER_FIXTURE_DIGEST_CALLS.load(std::sync::atomic::Ordering::Relaxed),
                1,
                "source and sealed candidate avoid additional full reads"
            );

            chmod_until_observable(&candidate, 0o700);
            validate_pin(&runtime).expect("chmod preserves valid candidate bytes");
            assert_eq!(
                TEST_CONTROLLER_FIXTURE_DIGEST_CALLS.load(std::sync::atomic::Ordering::Relaxed),
                2,
                "chmod invalidates metadata identity and performs one rehash"
            );

            fs::write(&candidate, b"mutated candidate").expect("mutate candidate");
            assert!(
                validate_pin(&runtime).is_err(),
                "mutated candidate fails closed"
            );
            assert_eq!(
                TEST_CONTROLLER_FIXTURE_DIGEST_CALLS.load(std::sync::atomic::Ordering::Relaxed),
                3,
                "content mutation misses the cache and rehashes before failing"
            );

            fs::remove_file(&candidate).expect("remove candidate");
            fs::copy(
                crate::test_support::controller_runtime_test_executable(),
                &candidate,
            )
            .expect("replace candidate");
            fs::set_permissions(&candidate, fs::Permissions::from_mode(0o700))
                .expect("make replacement executable");
            fs::write(&candidate, b"replacement candidate").expect("replace candidate bytes");
            assert!(
                validate_pin(&runtime).is_err(),
                "replaced candidate fails closed"
            );
            assert_eq!(
                TEST_CONTROLLER_FIXTURE_DIGEST_CALLS.load(std::sync::atomic::Ordering::Relaxed),
                4,
                "replacement misses the cache and rehashes before failing"
            );
        });
    }

    /// Sealing one cook hashes the same controller executable up to seven times
    /// across two processes. Each of those is a full read of a binary that is
    /// hundreds of megabytes unoptimized, which measurably dominates a detached
    /// `agent-task cook`: 75.93s of a 76.92s acceptance run was spent inside the
    /// Cook CLI while every other phase of that run cost 0.64s combined
    /// (#10659).
    #[cfg(unix)]
    #[test]
    fn executable_digests_are_memoized_per_observed_file_identity() {
        use std::os::unix::fs::PermissionsExt;

        crate::test_support::with_isolated_home(|_| {
            EXECUTABLE_DIGESTS
                .get_or_init(|| Mutex::new(BTreeMap::new()))
                .lock()
                .expect("controller executable digest memo is not poisoned")
                .clear();
            EXECUTABLE_DIGEST_COMPUTATIONS.store(0, std::sync::atomic::Ordering::Relaxed);
            // This test is about the memo, not about the settle guard: a
            // unit-test-sized file can never hash for long enough to be
            // memoized under the production threshold. Restored on drop so a
            // panic here cannot silently re-enable memoization elsewhere.
            let _settle_override = SettleThresholdOverride::of_millis(0);

            let temporary = tempfile::tempdir().expect("temporary executable directory");
            let executable = temporary.path().join("homeboy");
            fs::write(&executable, b"controller bytes").expect("write controller");
            fs::set_permissions(&executable, fs::Permissions::from_mode(0o755))
                .expect("make controller executable");

            let first = executable_digest(&executable).expect("hash controller");
            let second = executable_digest(&executable).expect("rehash controller");
            assert_eq!(first, second);
            assert_eq!(
                EXECUTABLE_DIGEST_COMPUTATIONS.load(std::sync::atomic::Ordering::Relaxed),
                1,
                "an unchanged executable is read once per process"
            );

            // A memo that survived a content change would report the previous
            // digest for different bytes, which is the one failure mode that
            // matters: it would let a replaced controller validate.
            fs::write(&executable, b"different controller bytes").expect("mutate controller");
            let mutated = executable_digest(&executable).expect("hash mutated controller");
            assert_ne!(
                mutated, first,
                "a mutated executable never reuses its prior digest"
            );
            assert_eq!(
                EXECUTABLE_DIGEST_COMPUTATIONS.load(std::sync::atomic::Ordering::Relaxed),
                2,
                "mutation misses the memo and rehashes"
            );

            assert_eq!(
                executable_digest(&executable).expect("rehash mutated controller"),
                mutated
            );
            assert_eq!(
                EXECUTABLE_DIGEST_COMPUTATIONS.load(std::sync::atomic::Ordering::Relaxed),
                2,
                "the mutated executable is then memoized under its new identity"
            );
        });
    }

    /// Publishing a pin verifies the staged bytes and then validates the
    /// published pin, which used to read the same file twice more. Sealing the
    /// pin's mode and linking it both move the inode's change time, so the
    /// memo has to be seeded from the destination as publication left it.
    #[cfg(unix)]
    #[test]
    fn publishing_a_pin_seeds_the_digest_its_validation_needs() {
        use std::os::unix::fs::PermissionsExt;

        crate::test_support::with_isolated_home(|_| {
            // `pin_executable` validates the pin it publishes, and validation
            // resolves the test-support controller fixture's digest. That
            // fixture cache is process-wide state this test does not own, so
            // warm it deliberately: the count below is meant to measure
            // publication, not whether a neighbouring test happened to warm the
            // fixture first. Leaving it to chance is why this passed under
            // `cargo test` and failed under nextest, which runs every test in
            // its own process (#12226).
            warm_test_controller_fixture_digest();

            EXECUTABLE_DIGESTS
                .get_or_init(|| Mutex::new(BTreeMap::new()))
                .lock()
                .expect("controller executable digest memo is not poisoned")
                .clear();
            EXECUTABLE_DIGEST_COMPUTATIONS.store(0, std::sync::atomic::Ordering::Relaxed);

            let temporary = tempfile::tempdir().expect("temporary executable directory");
            let source = temporary.path().join("homeboy");
            fs::write(
                &source,
                "#!/bin/sh\nif [ \"$1\" = self ] && [ \"$2\" = identity ]; then\n  printf '%s\\n' '{\"data\":{\"display\":\"homeboy 0.0.0+fixture\"}}'\n  exit 0\nfi\nexit 1\n",
            )
            .expect("write controller");
            fs::set_permissions(&source, fs::Permissions::from_mode(0o755))
                .expect("make controller executable");

            let runtime = pin_executable(&source, "homeboy 0.0.0+fixture").expect("pin executable");
            assert_eq!(
                EXECUTABLE_DIGEST_COMPUTATIONS.load(std::sync::atomic::Ordering::Relaxed),
                2,
                "publication reads the source and the staged copy, and nothing else"
            );

            validate_pin(&runtime).expect("published pin validates");
            assert_eq!(
                EXECUTABLE_DIGEST_COMPUTATIONS.load(std::sync::atomic::Ordering::Relaxed),
                2,
                "revalidating a published pin resolves from the memo"
            );

            // The memo must not survive the pin being replaced underneath it,
            // which is the whole reason the pin is validated at all.
            let pinned = runtime
                .pointer("/originating/pinned_executable")
                .and_then(Value::as_str)
                .map(PathBuf::from)
                .expect("pinned executable");
            fs::set_permissions(&pinned, fs::Permissions::from_mode(0o700))
                .expect("unseal pinned executable");
            fs::write(&pinned, b"tampered controller bytes").expect("tamper pinned executable");
            assert!(
                validate_pin(&runtime).is_err(),
                "a tampered pin fails closed rather than reusing its memoized digest"
            );
        });
    }

    #[cfg(unix)]
    #[test]
    fn pinned_runtime_executes_original_controller_after_global_binary_replacement() {
        use std::os::unix::fs::PermissionsExt;

        crate::test_support::with_isolated_home(|_| {
            let temporary = tempfile::tempdir().expect("temporary executable directory");
            let global = temporary.path().join("homeboy");
            let write_controller = |identity: &str| {
                let identity = serde_json::to_string(identity).expect("serialize identity");
                fs::write(
                    &global,
                    format!(
                        "#!/bin/sh\nif [ \"$1\" = self ] && [ \"$2\" = identity ]; then\n  printf '%s\\n' '{{\"data\":{{\"display\":{identity}}}}}'\n  exit 0\nfi\nif [ \"$1\" = controller ] && [ \"$2\" = admission ]; then\n  printf '%s\\n' {identity}\n  exit 0\nfi\nexit 1\n"
                    ),
                )
                .expect("write global controller");
                fs::set_permissions(&global, fs::Permissions::from_mode(0o755))
                    .expect("make global controller executable");
            };

            write_controller("homeboy 0.288.13+original");
            let runtime = pin_executable(&global, "homeboy 0.288.13+original")
                .expect("pin original controller");
            let pinned = runtime
                .pointer("/originating/pinned_executable")
                .and_then(Value::as_str)
                .expect("pinned executable");

            // Simulate a concurrent global install after pin creation and before admission.
            write_controller("homeboy 0.288.13+replacement");
            let output = Command::new(pinned)
                .args(["controller", "admission"])
                .output()
                .expect("execute pinned controller admission");

            assert!(output.status.success());
            assert_eq!(
                String::from_utf8_lossy(&output.stdout).trim(),
                "homeboy 0.288.13+original"
            );
            assert_eq!(
                executable_identity(&global, None).expect("global replacement identity"),
                "homeboy 0.288.13+replacement"
            );
        });
    }

    #[test]
    fn publication_is_no_clobber_and_idempotent() {
        let temporary = tempfile::tempdir().expect("temporary runtime directory");
        let source = temporary.path().join("source");
        let destination = temporary.path().join("runtime/homeboy");
        fs::write(&source, b"generation-a").expect("write source");
        let digest = executable_digest(&source).expect("hash source");

        publish_pin(&source, &destination, &digest).expect("publish first pin");
        publish_pin(&source, &destination, &digest).expect("reuse exact pin");
        fs::write(&source, b"generation-b").expect("replace source");
        let error = publish_pin(
            &source,
            &destination,
            &executable_digest(&source).expect("hash replacement"),
        )
        .expect_err("different bytes must never replace a pin");

        assert!(error.message.contains("different bytes"));
        assert_eq!(fs::read(&destination).expect("read pin"), b"generation-a");
    }

    #[test]
    fn concurrent_publication_is_no_clobber_and_idempotent() {
        let temporary = tempfile::tempdir().expect("temporary runtime directory");
        let source = temporary.path().join("source");
        let destination = temporary.path().join("runtime/homeboy");
        fs::write(&source, b"generation-a").expect("write source");
        let digest = executable_digest(&source).expect("hash source");

        std::thread::scope(|scope| {
            let mut publications = Vec::new();
            for _ in 0..8 {
                publications.push(scope.spawn(|| publish_pin(&source, &destination, &digest)));
            }
            for publication in publications {
                publication
                    .join()
                    .expect("publication thread completes")
                    .expect("concurrent publication succeeds");
            }
        });
        assert_eq!(fs::read(&destination).expect("read pin"), b"generation-a");
    }

    #[cfg(unix)]
    #[test]
    fn legacy_v1_pin_migration_publishes_before_returning_updated_metadata() {
        crate::test_support::with_isolated_home(|_| {
            let temporary = tempfile::tempdir().expect("temporary runtime directory");
            let legacy = temporary.path().join("legacy-homeboy");
            let identity = "homeboy test+legacy";
            fake_controller(&legacy, identity, "legacy");
            let runtime = json!({ "originating": {
                "build_identity": identity,
                "pinned_executable": legacy,
            }});

            let migrated = migrate_legacy_pin_in_root(&test_runtime_root(), &runtime)
                .expect("migrate legacy pin");
            let destination = PathBuf::from(
                migrated["originating"]["pinned_executable"]
                    .as_str()
                    .expect("migrated path"),
            );
            assert_ne!(destination, legacy);
            assert!(legacy.exists());
            assert!(destination.is_file());
            assert_eq!(migrated["schema"], "homeboy/controller-runtime-pin/v2");
            assert_eq!(migrated["requested"], identity);
            assert_eq!(migrated["current"], identity);
            assert_eq!(migrated["executed"], identity);
            validate_pin(&migrated).expect("migrated pin validates");
        });
    }

    #[test]
    fn pin_diagnostics_distinguish_missing_and_hash_mismatch() {
        let temporary = tempfile::tempdir().expect("temporary runtime directory");
        let pinned = temporary.path().join("homeboy");
        fs::write(&pinned, b"generation-a").expect("write pin");
        make_executable_read_only(&pinned).expect("seal pin");
        let runtime = json!({ "originating": { "pinned_executable": pinned, "sha256": "00" } });
        let mismatch = validate_pin(&runtime).expect_err("hash mismatch");
        assert!(mismatch.message.contains("hash mismatch"));
        fs::remove_file(
            runtime
                .pointer("/originating/pinned_executable")
                .and_then(Value::as_str)
                .expect("path"),
        )
        .expect("remove pin");
        let missing = validate_pin(&runtime).expect_err("missing pin");
        assert!(missing.message.contains("missing"));
    }

    #[cfg(unix)]
    #[test]
    fn artifact_diagnostics_distinguish_missing_non_executable_hash_and_identity() {
        use std::os::unix::fs::PermissionsExt;

        let temporary = tempfile::tempdir().expect("temporary runtime directory");
        let artifact = temporary.path().join("homeboy");
        let missing = verify_artifact(&artifact, "00", "homeboy test+one")
            .expect_err("missing artifact fails");
        assert!(missing.message.contains("missing"));

        fs::write(&artifact, b"not executable").expect("write artifact");
        let non_executable = verify_artifact(&artifact, "00", "homeboy test+one")
            .expect_err("non-executable artifact fails");
        assert!(non_executable.message.contains("not executable"));

        let digest = fake_controller(&artifact, "homeboy test+one", "artifact");
        let hash =
            verify_artifact(&artifact, "00", "homeboy test+one").expect_err("hash mismatch fails");
        assert!(hash.message.contains("hash mismatch"));
        let identity = verify_artifact(&artifact, &digest, "homeboy test+two")
            .expect_err("identity mismatch fails");
        assert!(identity.message.contains("build identity mismatch"));
        fs::set_permissions(&artifact, fs::Permissions::from_mode(0o500)).expect("seal artifact");
    }

    #[cfg(unix)]
    #[test]
    fn durable_pin_rejects_a_matching_hash_with_the_wrong_build_identity() {
        let temporary = tempfile::tempdir().expect("temporary runtime directory");
        let pinned = temporary.path().join("homeboy");
        let digest = fake_controller(&pinned, "homeboy 0.288.4+older", "wrong identity");
        make_executable_read_only(&pinned).expect("seal executable");
        let runtime = json!({ "originating": {
            "build_identity": "homeboy 0.288.6+expected",
            "pinned_executable": pinned,
            "sha256": digest,
        }});

        let error =
            validate_pin(&runtime).expect_err("identity mismatch must fail after hash validation");

        assert!(error.message.contains("build identity mismatch"));
        assert!(error.message.contains("homeboy 0.288.6+expected"));
        assert!(error.message.contains("homeboy 0.288.4+older"));
    }

    #[cfg(unix)]
    #[test]
    fn recovery_preserves_runtime_a_after_generation_b_activation() {
        use std::os::unix::fs::PermissionsExt;

        crate::test_support::with_isolated_home(|_| {
            let temporary = tempfile::tempdir().expect("temporary controller directory");
            let artifact_a = temporary.path().join("homeboy-a");
            let artifact_b = temporary.path().join("homeboy-b");
            let identity_a = "homeboy test+runtime-a";
            let identity_b = "homeboy test+runtime-b";
            let digest_a = fake_controller(&artifact_a, identity_a, "runtime A");
            let digest_b = fake_controller(&artifact_b, identity_b, "runtime B");
            let pin_a = pinned_path(identity_a, &digest_a).expect("runtime A path");
            let pin_b = pinned_path(identity_b, &digest_b).expect("runtime B path");
            publish_pin(&artifact_a, &pin_a, &digest_a).expect("publish runtime A");
            let runtime_a = json!({ "originating": {
                "build_identity": identity_a,
                "pinned_executable": pin_a,
                "sha256": digest_a,
            }});
            validate_pin(&runtime_a).expect("runtime A validates before upgrade");
            let runtime_a_bytes = fs::read(&pin_a).expect("read runtime A");

            publish_pin(&artifact_b, &pin_b, &digest_b).expect("publish runtime B");
            let runtime_b = json!({ "originating": {
                "build_identity": identity_b,
                "pinned_executable": pin_b,
                "sha256": digest_b,
            }});
            write_active_generation(
                &runtime_root()
                    .expect("runtime root")
                    .join(ACTIVE_GENERATION_FILE),
                &runtime_b,
            )
            .expect("activate runtime B");
            assert_eq!(
                fs::read(&pin_a).expect("read runtime A after upgrade"),
                runtime_a_bytes
            );
            validate_pin(&runtime_a)
                .expect("runtime A remains executable after runtime B activation");

            fs::set_permissions(&pin_a, fs::Permissions::from_mode(0o700))
                .expect("allow test corruption");
            fs::write(&pin_a, b"corrupted runtime A").expect("corrupt runtime A");
            let error = validate_pin(&runtime_a).expect_err("corruption fails closed");
            assert!(error.message.contains("hash mismatch"));
            assert!(error.message.contains(&digest_a));

            let recovered =
                recover_pin_in_root(&test_runtime_root(), &runtime_a, Some(&artifact_a), None)
                    .expect("recover runtime A from trusted artifact");
            let recovered_pin = PathBuf::from(
                recovered["originating"]["pinned_executable"]
                    .as_str()
                    .expect("recovered runtime A path"),
            );
            assert_ne!(recovered_pin, pin_a);
            assert_eq!(
                fs::read(&recovered_pin).expect("read recovered runtime A"),
                runtime_a_bytes
            );
            validate_pin(&recovered).expect("recovered runtime A validates");
        });
    }

    #[cfg(unix)]
    #[test]
    fn cleanup_preserves_active_generation_and_reclaims_unpinned_identity_under_size_pressure() {
        crate::test_support::with_isolated_home(|_| {
            let temporary = tempfile::tempdir().expect("temporary controller directory");
            let current = temporary.path().join("current");
            let stale = temporary.path().join("stale");
            let current_digest = fake_controller(&current, "homeboy test+current", "current");
            let stale_digest = fake_controller(&stale, "homeboy test+stale", "stale");
            let current_pin =
                pinned_path("homeboy test+current", &current_digest).expect("current path");
            let stale_pin = pinned_path("homeboy test+stale", &stale_digest).expect("stale path");
            publish_pin(&current, &current_pin, &current_digest).expect("publish current");
            publish_pin(&stale, &stale_pin, &stale_digest).expect("publish stale");
            write_active_generation(
                &runtime_root().expect("root").join(ACTIVE_GENERATION_FILE),
                &json!({ "originating": { "pinned_executable": current_pin } }),
            )
            .expect("activate current");

            let inventory = cleanup(ControllerRuntimeCleanupOptions {
                apply: false,
                min_age: Duration::from_secs(u64::MAX),
                max_total_bytes: 0,
                limit: 10,
            })
            .expect("inventory");
            assert!(inventory
                .snapshots
                .iter()
                .any(|snapshot| snapshot.pins.contains(&current_pin) && !snapshot.eligible));
            assert!(inventory
                .snapshots
                .iter()
                .any(|snapshot| snapshot.pins.contains(&stale_pin) && snapshot.eligible));
            let applied = cleanup(ControllerRuntimeCleanupOptions {
                apply: true,
                min_age: Duration::from_secs(u64::MAX),
                max_total_bytes: 0,
                limit: 10,
            })
            .expect("apply");
            assert!(applied.removed.contains(&stale_pin));
            assert!(current_pin.exists());
            assert!(!stale_pin.exists());
        });
    }

    #[cfg(unix)]
    fn save_retention_config(retention: crate::defaults::RetentionConfig) {
        crate::defaults::save_config(&crate::defaults::HomeboyConfig {
            retention,
            ..crate::defaults::HomeboyConfig::default()
        })
        .expect("save retention config");
    }

    #[test]
    fn resolved_prune_policy_reads_configuration_and_purges_only_on_opt_in() {
        let retention = crate::defaults::RetentionConfig {
            controller_runtime_days: 14,
            controller_runtime_max_bytes: 4096,
            limit: 7,
            ..crate::defaults::RetentionConfig::default()
        };

        let configured = cleanup_options_from_retention(
            true,
            ControllerRuntimeRetentionOverrides::default(),
            &retention,
        );
        assert_eq!(configured.min_age, Duration::from_secs(14 * 86_400));
        assert_eq!(configured.max_total_bytes, 4096);
        assert_eq!(configured.limit, 7);

        let overridden = cleanup_options_from_retention(
            true,
            ControllerRuntimeRetentionOverrides {
                limit: Some(2),
                ignore_retention: false,
            },
            &retention,
        );
        assert_eq!(overridden.limit, 2);
        assert_eq!(overridden.min_age, configured.min_age);
        assert_eq!(overridden.max_total_bytes, configured.max_total_bytes);

        let purge = cleanup_options_from_retention(
            true,
            ControllerRuntimeRetentionOverrides {
                limit: Some(2),
                ignore_retention: true,
            },
            &retention,
        );
        assert_eq!(purge.min_age, Duration::ZERO);
        assert_eq!(purge.max_total_bytes, 0);
        assert_eq!(purge.limit, usize::MAX);

        let negative = cleanup_options_from_retention(
            true,
            ControllerRuntimeRetentionOverrides {
                limit: Some(-1),
                ignore_retention: false,
            },
            &retention,
        );
        assert_eq!(negative.limit, 0);
    }

    #[cfg(unix)]
    #[test]
    fn prune_pins_honors_the_configured_window_until_a_purge_is_requested() {
        crate::test_support::with_isolated_home(|_| {
            save_retention_config(crate::defaults::RetentionConfig {
                controller_runtime_days: 3_650,
                controller_runtime_max_bytes: u64::MAX,
                limit: 10,
                ..crate::defaults::RetentionConfig::default()
            });
            let temporary = tempfile::tempdir().expect("temporary controller directory");
            let stale = temporary.path().join("stale");
            let digest = fake_controller(&stale, "homeboy test+stale", "stale");
            let pin = pinned_path("homeboy test+stale", &digest).expect("stale path");
            publish_pin(&stale, &pin, &digest).expect("publish stale");

            // Unreferenced, so eligible — but still inside the operator's
            // configured age and size budget, so it must survive.
            let planned = prune_pins(false, ControllerRuntimeRetentionOverrides::default())
                .expect("plan inside configured window");
            assert!(planned.eligible.contains(&pin));
            let applied = prune_pins(true, ControllerRuntimeRetentionOverrides::default())
                .expect("apply inside configured window");
            assert!(applied.removed.is_empty());
            assert!(pin.exists());

            let purged = prune_pins(
                true,
                ControllerRuntimeRetentionOverrides {
                    limit: None,
                    ignore_retention: true,
                },
            )
            .expect("explicit policy-free purge");
            assert!(purged.removed.contains(&pin));
            assert!(!pin.exists());
        });
    }

    #[cfg(unix)]
    #[test]
    fn prune_pins_bounds_removals_by_the_configured_limit() {
        crate::test_support::with_isolated_home(|_| {
            save_retention_config(crate::defaults::RetentionConfig {
                controller_runtime_days: 0,
                controller_runtime_max_bytes: u64::MAX,
                limit: 1,
                ..crate::defaults::RetentionConfig::default()
            });
            let temporary = tempfile::tempdir().expect("temporary controller directory");
            let pins = (0..3)
                .map(|index| {
                    let artifact = temporary.path().join(format!("stale-{index}"));
                    let identity = format!("homeboy test+stale-{index}");
                    let digest = fake_controller(&artifact, &identity, &format!("stale {index}"));
                    let pin = pinned_path(&identity, &digest).expect("stale path");
                    publish_pin(&artifact, &pin, &digest).expect("publish stale");
                    pin
                })
                .collect::<Vec<_>>();

            let applied = prune_pins(true, ControllerRuntimeRetentionOverrides::default())
                .expect("apply configured limit");

            assert_eq!(applied.removed_identities.len(), 1);
            assert_eq!(pins.iter().filter(|pin| pin.exists()).count(), 2);
        });
    }

    #[test]
    fn cleanup_dry_run_preserves_tombstones_and_apply_recovers_them() {
        crate::test_support::with_isolated_home(|_| {
            let root = runtime_root().expect("runtime root");
            let tombstone = root.join(".cleanup-interrupted");
            fs::create_dir_all(&tombstone).expect("create tombstone");
            fs::write(tombstone.join("homeboy"), b"interrupted").expect("write tombstone");

            cleanup(ControllerRuntimeCleanupOptions {
                apply: false,
                min_age: Duration::ZERO,
                max_total_bytes: 0,
                limit: 1,
            })
            .expect("dry run");
            assert!(tombstone.exists());
            cleanup(ControllerRuntimeCleanupOptions {
                apply: true,
                min_age: Duration::ZERO,
                max_total_bytes: 0,
                limit: 1,
            })
            .expect("apply");
            assert!(!tombstone.exists());
        });
    }

    #[test]
    fn concurrent_pin_current_queued_requests_queue_and_succeed_in_fifo_order() {
        crate::test_support::with_isolated_home(|_| {
            let first = admit_current_for("existing-owner").expect("hold initial admission");

            let (a_enqueued, a_enqueued_result) = std::sync::mpsc::channel();
            let (a_release, a_release_result) = std::sync::mpsc::channel();
            let seal_a = std::thread::spawn(move || match admit_current_for("seal-a") {
                Ok(admission) => {
                    let _ = a_enqueued.send(Ok(()));
                    let _ = a_release_result.recv();
                    drop(admission);
                }
                Err(error) => {
                    let _ = a_enqueued.send(Err(error.message));
                }
            });

            let waiting_a = (0..40)
                .map(|_| {
                    let status = admission_status("seal-a").expect("seal-a status");
                    if status["state"] == "waiting" {
                        Some(status)
                    } else {
                        std::thread::sleep(Duration::from_millis(25));
                        None
                    }
                })
                .find_map(|status| status)
                .expect("seal-a queues behind existing owner");
            assert_eq!(waiting_a["position"], 2);

            // Enqueue B only after A is durably visible, so this asserts FIFO
            // order rather than scheduler order between two spawned threads.
            let (b_enqueued, b_enqueued_result) = std::sync::mpsc::channel();
            let (b_release, b_release_result) = std::sync::mpsc::channel();
            let b_handle = std::thread::spawn(move || match admit_current_for("seal-b") {
                Ok(admission) => {
                    let _ = b_enqueued.send(Ok(()));
                    let _ = b_release_result.recv();
                    drop(admission);
                }
                Err(error) => {
                    let _ = b_enqueued.send(Err(error.message));
                }
            });

            let waiting_b = (0..40)
                .map(|_| {
                    let status = admission_status("seal-b").expect("seal-b status");
                    if status["state"] == "waiting" {
                        Some(status)
                    } else {
                        std::thread::sleep(Duration::from_millis(25));
                        None
                    }
                })
                .find_map(|status| status)
                .expect("seal-b queues behind seal-a");
            assert_eq!(waiting_b["position"], 3);

            drop(first);

            assert_eq!(
                a_enqueued_result
                    .recv_timeout(Duration::from_secs(30))
                    .expect("seal-a resolves"),
                Ok(())
            );
            assert_eq!(
                admission_status("seal-a").expect("seal-a admitted")["state"],
                "admitted"
            );
            a_release.send(()).expect("release seal-a");
            seal_a.join().expect("seal-a thread exits");

            assert_eq!(
                b_enqueued_result
                    .recv_timeout(Duration::from_secs(30))
                    .expect("seal-b resolves"),
                Ok(())
            );
            assert_eq!(
                admission_status("seal-b").expect("seal-b admitted")["state"],
                "admitted"
            );
            b_release.send(()).expect("release seal-b");
            b_handle.join().expect("seal-b thread exits");
        });
    }
}
