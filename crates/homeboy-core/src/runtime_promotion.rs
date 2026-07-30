//! Machine-global serialization for operations that replace or select Homeboy
//! binaries. The directory-create guard follows the established rig lease lock
//! convention, while the JSON record makes a blocked writer actionable.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use fs4::fs_std::FileExt;
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;
use std::time::Instant;

use crate::build_identity;
use crate::error::{Error, ErrorCode, Result};
use crate::paths;

const LEASE_DIR: &str = "promotion.lock";
const LEASE_FILE: &str = "lease.json";
const ADMISSION_LOCK_FILE: &str = "admission.lock";
const PIN_DIR: &str = "pins";
const DEFAULT_TTL: Duration = Duration::from_secs(30 * 60);
const PIN_DRAIN_POLL: Duration = Duration::from_millis(100);
const SUBPROCESS_LEASE_ENV: &str = "HOMEBOY_RUNTIME_PROMOTION_LEASE";
// Directory creation publishes the lock before its atomically-renamed record.
// Give concurrent readers a bounded window to observe that publication.
const ACQUIRE_DISAPPEARED_LEASE_RETRIES: usize = 20;
const COMPATIBLE_WAIT_POLL: Duration = Duration::from_millis(50);
const COMPATIBLE_WAIT_HEARTBEAT: Duration = if cfg!(test) {
    Duration::from_millis(10)
} else {
    Duration::from_secs(5)
};

/// Observable state for a caller waiting on a compatible runtime mutation.
///
/// Runtime promotion is machine-scoped, while `target` keeps unrelated runner
/// or runtime resources independent. Callers render this event in their own
/// output protocol rather than making core depend on a CLI format.
#[derive(Debug, Clone, Serialize)]
pub struct RuntimePromotionWaitEvent {
    pub schema: &'static str,
    pub state: &'static str,
    pub resource_class: &'static str,
    pub wait_timeout_ms: u128,
    pub waited_ms: u128,
    pub owner_pid: u32,
    pub owner_operation: String,
    pub target: String,
    pub owner_generation: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimePromotionLeaseRecord {
    pub schema: String,
    pub pid: u32,
    pub operation: String,
    pub target: String,
    pub generation: String,
    /// Non-secret result fingerprint used for exact compatible-owner waits.
    /// Error diagnostics expose it, so callers must use immutable revision IDs
    /// or hashes rather than credentials or request payloads.
    #[serde(default)]
    pub compatibility_key: String,
    pub started_at: String,
    /// Linux start ticks fence PID reuse. Platforms that cannot supply this
    /// evidence retain a live PID as a fail-closed owner.
    #[serde(default)]
    pub linux_starttime_ticks: Option<u64>,
    /// Cross-platform process-start identity for capability handoff. The
    /// legacy Linux ticks remain populated for v2 readers during rollout.
    #[serde(default)]
    pub process_start_identity: Option<crate::process::ProcessStartIdentity>,
    /// Written at transaction boundaries. Expiry is diagnostic only: it never
    /// authorizes stealing from a process still proven live.
    #[serde(default)]
    pub heartbeat_at: String,
    #[serde(default)]
    pub expires_at: String,
    /// A random capability is required when the transaction crosses a process
    /// boundary. The promotion directory is already local-user state.
    #[serde(default)]
    pub capability: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SubprocessLeaseCapability {
    owner_pid: u32,
    #[serde(default)]
    owner_linux_starttime_ticks: Option<u64>,
    #[serde(default)]
    owner_process_start_identity: Option<crate::process::ProcessStartIdentity>,
    target: String,
    generation: String,
    capability: String,
}

thread_local! {
    // Direct reentrancy is deliberately capability-based. A matching PID alone
    // is not ownership evidence because PIDs are reusable after a restart.
    static LOCAL_LEASE_CAPABILITIES: RefCell<Vec<SubprocessLeaseCapability>> = const { RefCell::new(Vec::new()) };
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RuntimeGenerationPin {
    pid: u32,
    cook_id: String,
    generation: String,
    started_at: String,
}

/// Held while a controller/runner runtime transaction is in progress.
#[derive(Debug)]
pub struct RuntimePromotionLease {
    path: PathBuf,
    primary: bool,
    generation: String,
    target: String,
    compatibility_key: String,
    owner_pid: u32,
    capability: String,
    // Held from reservation through promotion completion. Pin creation takes a
    // shared lock on this inode, so no old-generation cook can slip in while
    // this promotion waits for already-pinned work to drain.
    admission_lock: Option<fs::File>,
}

/// Pins the generation required by a cook until its lifecycle finalizes.
#[derive(Debug)]
pub struct RuntimeGenerationPinGuard {
    path: PathBuf,
}

#[derive(Debug, Clone, Serialize)]
pub struct RuntimePromotionTakeover {
    pub previous: RuntimePromotionLeaseRecord,
    pub archived_path: String,
}

enum LeaseRecordReadError {
    Disappeared(std::io::Error),
    Failure(Error),
}

impl Drop for RuntimePromotionLease {
    fn drop(&mut self) {
        if self.primary {
            forget_local_capability(
                self.owner_pid,
                &self.target,
                &self.generation,
                &self.capability,
            );
            // Never remove a lease that an explicit takeover or another owner
            // has replaced since this guard was acquired.
            if read_record(&self.path).is_ok_and(|record| {
                record.pid == self.owner_pid
                    && record.target == self.target
                    && record.generation == self.generation
                    && record.compatibility_key == self.compatibility_key
                    && record.capability == self.capability
            }) {
                let _ = fs::remove_dir_all(&self.path);
            }
        }
    }
}

impl Drop for RuntimeGenerationPinGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// Acquire the global writer lease. A nested call must retain the same target
/// and generation. A child process must present the capability explicitly
/// attached by [`RuntimePromotionLease::authorize_subprocess`].
pub fn acquire(operation: &str, target: impl Into<String>) -> Result<RuntimePromotionLease> {
    acquire_with_pin_policy(
        operation,
        target.into(),
        String::new(),
        ForeignPinPolicy::Block,
    )
}

/// Wait for a compatible owner to finish, then acquire the writer lease.
///
/// Only an owner targeting the same runtime generation and target is eligible
/// for this handoff. Other promotions retain immediate, fail-closed contention
/// behavior. The wait has no persisted queue entry, so a cancelled caller
/// cannot strand later contenders.
pub fn acquire_waiting_for_compatible(
    operation: &str,
    target: impl Into<String>,
    timeout: Duration,
    progress: impl FnMut(RuntimePromotionWaitEvent),
) -> Result<RuntimePromotionLease> {
    acquire_waiting_for_compatible_key(operation, target, "", timeout, progress)
}

/// Wait only for an owner with an exactly matching opaque result key.
pub fn acquire_waiting_for_compatible_key(
    operation: &str,
    target: impl Into<String>,
    compatibility_key: impl Into<String>,
    timeout: Duration,
    mut progress: impl FnMut(RuntimePromotionWaitEvent),
) -> Result<RuntimePromotionLease> {
    let target = target.into();
    let compatibility_key = compatibility_key.into();
    let generation = current_generation();
    let deadline = Instant::now() + timeout;
    let mut last_owner = None;
    let mut last_progress = None;

    loop {
        match acquire_with_pin_policy(
            operation,
            target.clone(),
            compatibility_key.clone(),
            ForeignPinPolicy::Block,
        ) {
            Ok(lease) => return Ok(lease),
            Err(error) if is_contention_error(&error) => {
                let owner = contention_owner(&error)?;
                if !is_compatible_owner(&owner, &target, &generation, &compatibility_key) {
                    return Err(error);
                }
                let owner_identity = format!("{}:{}:{}", owner.pid, owner.operation, owner.target);
                if last_owner.as_ref() != Some(&owner_identity)
                    || last_progress
                        .is_none_or(|last: Instant| last.elapsed() >= COMPATIBLE_WAIT_HEARTBEAT)
                {
                    progress(RuntimePromotionWaitEvent {
                        schema: "homeboy/runtime-promotion-admission/v1",
                        state: "queued",
                        resource_class: "runtime_promotion",
                        wait_timeout_ms: timeout.as_millis(),
                        waited_ms: timeout
                            .saturating_sub(deadline.saturating_duration_since(Instant::now()))
                            .as_millis(),
                        owner_pid: owner.pid,
                        owner_operation: owner.operation.clone(),
                        target: owner.target.clone(),
                        owner_generation: owner.generation.clone(),
                    });
                    last_owner = Some(owner_identity);
                    last_progress = Some(Instant::now());
                }
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Err(wait_timeout_error(&owner, timeout));
                }
                std::thread::sleep(remaining.min(COMPATIBLE_WAIT_POLL));
            }
            Err(error) => return Err(error),
        }
    }
}

/// Acquire the global writer lease for a generation-preserving rotation.
///
/// The caller must keep existing work routed to its pinned generation while a
/// validated candidate becomes the owner of future admissions. The writer
/// lease still serializes concurrent mutations; only the controller Cook pin
/// barrier is relaxed for this transaction.
pub fn acquire_for_generation_rotation(
    operation: &str,
    target: impl Into<String>,
) -> Result<RuntimePromotionLease> {
    acquire_with_pin_policy(
        operation,
        target.into(),
        String::new(),
        ForeignPinPolicy::Allow,
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ForeignPinPolicy {
    Block,
    Allow,
}

fn acquire_with_pin_policy(
    operation: &str,
    target: String,
    compatibility_key: String,
    foreign_pin_policy: ForeignPinPolicy,
) -> Result<RuntimePromotionLease> {
    let root = paths::runtime_promotion_dir()?;
    fs::create_dir_all(&root).map_err(io("create runtime promotion directory"))?;
    let path = root.join(LEASE_DIR);
    let pid = std::process::id();
    let generation = current_generation();
    let subprocess_capability = subprocess_capability_from_env();
    let mut lease = match acquire_lease_dir_with_retry(
        || create_lease_dir(&path),
        || read_record_for_acquisition(&path),
    )? {
        None => {
            let capability = uuid::Uuid::new_v4().to_string();
            let record = RuntimePromotionLeaseRecord {
                schema: "homeboy/runtime-promotion-lease/v2".to_string(),
                pid,
                operation: operation.to_string(),
                target: target.clone(),
                generation: generation.clone(),
                compatibility_key: compatibility_key.clone(),
                started_at: now(),
                linux_starttime_ticks: crate::process::linux_process_starttime_ticks(pid)
                    .ok()
                    .flatten(),
                process_start_identity: crate::process::process_start_identity(pid).ok().flatten(),
                heartbeat_at: now(),
                expires_at: expiry(),
                capability: capability.clone(),
            };
            if let Err(error) = write_record(&path, &record) {
                // A concurrent stale-owner recovery can remove the directory
                // after mkdir and before publication. Retry from acquisition;
                // never return an unpublished guard.
                if !path.exists() {
                    return acquire_with_pin_policy(
                        operation,
                        target,
                        compatibility_key,
                        foreign_pin_policy,
                    );
                }
                return Err(error);
            }
            remember_local_capability(&record)?;
            RuntimePromotionLease {
                path,
                primary: true,
                generation,
                target,
                compatibility_key,
                owner_pid: pid,
                capability,
                admission_lock: None,
            }
        }
        Some(held) => {
            if authorizes_local_reentrancy(&held, &target, &generation) {
                return Ok(RuntimePromotionLease {
                    path,
                    primary: false,
                    generation,
                    target,
                    compatibility_key: held.compatibility_key.clone(),
                    owner_pid: held.pid,
                    capability: held.capability,
                    admission_lock: None,
                });
            }
            if subprocess_capability.as_ref().is_some_and(|capability| {
                authorizes_subprocess(&held, &target, &generation, capability)
            }) {
                return Ok(RuntimePromotionLease {
                    path,
                    primary: false,
                    generation,
                    target,
                    compatibility_key: held.compatibility_key.clone(),
                    owner_pid: held.pid,
                    capability: held.capability,
                    admission_lock: None,
                });
            }
            if reclaimable(&held) {
                // Rename is the ownership CAS: one recovery moves the stale
                // directory while former owners cannot remove its replacement.
                match archive_stale_lease(&root, &path, &held) {
                    Ok(_) => {
                        return acquire_with_pin_policy(
                            operation,
                            target,
                            compatibility_key,
                            foreign_pin_policy,
                        )
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        return acquire_with_pin_policy(
                            operation,
                            target,
                            compatibility_key,
                            foreign_pin_policy,
                        )
                    }
                    Err(error) => return Err(io("archive stale runtime promotion lease")(error)),
                }
            }
            return Err(blocked_error(&held, false));
        }
    };

    if foreign_pin_policy == ForeignPinPolicy::Block && lease.primary {
        let admission_lock = open_admission_lock(&root)?;
        admission_lock
            .lock_exclusive()
            .map_err(io("reserve runtime promotion admission"))?;
        wait_for_foreign_generation_pins(&root, pid, subprocess_capability.as_ref())?;
        lease.admission_lock = Some(admission_lock);
    }
    Ok(lease)
}

/// Create the lease directory or return its existing record. A previous owner
/// can remove its directory after our create attempt observes it. Publication
/// also has a brief mkdir-to-record window, so readers retry within a bounded
/// interval when the record is not yet visible.
fn acquire_lease_dir_with_retry<Create, Read>(
    mut create: Create,
    mut read: Read,
) -> Result<Option<RuntimePromotionLeaseRecord>>
where
    Create: FnMut() -> std::io::Result<()>,
    Read: FnMut() -> std::result::Result<RuntimePromotionLeaseRecord, LeaseRecordReadError>,
{
    for attempt in 0..=ACQUIRE_DISAPPEARED_LEASE_RETRIES {
        match create() {
            Ok(()) => return Ok(None),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => match read() {
                Ok(record) => return Ok(Some(record)),
                Err(LeaseRecordReadError::Disappeared(_))
                    if attempt < ACQUIRE_DISAPPEARED_LEASE_RETRIES =>
                {
                    std::thread::sleep(Duration::from_millis(2));
                    continue;
                }
                Err(LeaseRecordReadError::Disappeared(error)) => {
                    return Err(io("read runtime promotion lease")(error));
                }
                Err(LeaseRecordReadError::Failure(error)) => return Err(error),
            },
            Err(error) => {
                return Err(Error::internal_io(
                    error.to_string(),
                    Some("acquire runtime promotion lease".to_string()),
                ));
            }
        }
    }

    unreachable!("the bounded acquisition loop always returns")
}

impl RuntimePromotionLease {
    /// Explicitly authorize one Homeboy subprocess to join this transaction.
    /// Callers must use this immediately before spawning the participating
    /// Homeboy command rather than relying on unrelated runtime environment.
    pub fn authorize_subprocess(&self, command: &mut Command) {
        let capability = SubprocessLeaseCapability {
            owner_pid: self.owner_pid,
            owner_linux_starttime_ticks: crate::process::linux_process_starttime_ticks(
                self.owner_pid,
            )
            .ok()
            .flatten(),
            owner_process_start_identity: crate::process::process_start_identity(self.owner_pid)
                .ok()
                .flatten(),
            target: self.target.clone(),
            generation: self.generation.clone(),
            capability: self.capability.clone(),
        };
        let payload = serde_json::to_vec(&capability)
            .expect("runtime promotion subprocess capability serializes");
        command.env(SUBPROCESS_LEASE_ENV, URL_SAFE_NO_PAD.encode(payload));
    }

    /// Refuse to continue a multi-step mutation after another runtime generation
    /// became visible. This prevents parser/behavior contract mixing.
    pub fn assert_generation(&self) -> Result<()> {
        self.heartbeat()?;
        let current = current_generation();
        if current == self.generation {
            return Ok(());
        }
        Err(Error::validation_invalid_argument(
            "runtime_generation",
            format!(
                "Homeboy runtime generation drifted from `{}` to `{current}` during promotion",
                self.generation
            ),
            Some(current),
            Some(vec![
                "Retry the complete promotion transaction; no further mutation was performed."
                    .to_string(),
            ]),
        ))
    }

    /// Refresh the durable record without granting authority to a replacement.
    pub fn heartbeat(&self) -> Result<()> {
        if !self.primary {
            return Ok(());
        }
        let mut record = read_record(&self.path)?;
        if record.pid != self.owner_pid
            || record.target != self.target
            || record.generation != self.generation
            || record.compatibility_key != self.compatibility_key
            || record.capability != self.capability
        {
            return Err(Error::internal_unexpected(
                "runtime promotion lease ownership changed while heartbeating",
            ));
        }
        record.heartbeat_at = now();
        record.expires_at = expiry();
        write_record(&self.path, &record)
    }
}

/// Pin the current generation for the complete cook lifecycle. Promotion is
/// deliberately conservative: any live pin blocks a writer until finalization.
pub fn pin_cook_generation(cook_id: &str) -> Result<RuntimeGenerationPinGuard> {
    let promotion_root = paths::runtime_promotion_dir()?;
    fs::create_dir_all(&promotion_root).map_err(io("create runtime promotion directory"))?;
    let admission_lock = open_admission_lock(&promotion_root)?;
    admission_lock
        .lock_shared()
        .map_err(io("join runtime promotion admission"))?;
    let root = promotion_root.join(PIN_DIR);
    fs::create_dir_all(&root).map_err(io("create runtime generation pin directory"))?;
    prune_pins(&root)?;
    let pid = std::process::id();
    let path = root.join(format!(
        "{}-{}-{}.json",
        paths::sanitize_path_segment(cook_id),
        pid,
        uuid::Uuid::new_v4(),
    ));
    let pin = RuntimeGenerationPin {
        pid,
        cook_id: cook_id.to_string(),
        generation: current_generation(),
        started_at: now(),
    };
    fs::write(
        &path,
        serde_json::to_vec_pretty(&pin).map_err(|e| Error::internal_json(e.to_string(), None))?,
    )
    .map_err(io("write runtime generation pin"))?;
    Ok(RuntimeGenerationPinGuard { path })
}

fn open_admission_lock(root: &Path) -> Result<fs::File> {
    fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(root.join(ADMISSION_LOCK_FILE))
        .map_err(io("open runtime promotion admission"))
}

/// Archive, rather than delete, a proven dead promotion lease. Normal
/// acquisition performs the same idempotent recovery automatically.
pub fn takeover_stale_lease() -> Result<RuntimePromotionTakeover> {
    let root = paths::runtime_promotion_dir()?;
    let path = root.join(LEASE_DIR);
    let previous = read_record(&path)?;
    if !reclaimable(&previous) {
        return Err(blocked_error(&previous, false));
    }
    let archived = archive_stale_lease(&root, &path, &previous)
        .map_err(io("archive stale runtime promotion lease"))?;
    Ok(RuntimePromotionTakeover {
        previous,
        archived_path: archived.display().to_string(),
    })
}

fn archive_stale_lease(
    root: &Path,
    path: &Path,
    expected: &RuntimePromotionLeaseRecord,
) -> std::io::Result<PathBuf> {
    // Re-check immediately before rename so a replacement cannot be archived
    // based on an old contender snapshot.
    if !path.exists() {
        return Err(std::io::Error::from(std::io::ErrorKind::NotFound));
    }
    let current = read_record(path).map_err(|error| std::io::Error::other(error.to_string()))?;
    if current.pid != expected.pid
        || current.capability != expected.capability
        || current.target != expected.target
        || !reclaimable(&current)
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::WouldBlock,
            "runtime promotion lease is no longer reclaimable",
        ));
    }
    let archived = root.join(format!("promotion.stale.{}.lock", uuid::Uuid::new_v4()));
    fs::rename(path, &archived)?;
    Ok(archived)
}

fn write_record(path: &Path, record: &RuntimePromotionLeaseRecord) -> Result<()> {
    let payload =
        serde_json::to_vec_pretty(record).map_err(|e| Error::internal_json(e.to_string(), None))?;
    let temporary = path.join(format!(".{LEASE_FILE}.{}.tmp", uuid::Uuid::new_v4()));
    fs::write(&temporary, payload).map_err(io("write runtime promotion lease"))?;
    fs::rename(&temporary, path.join(LEASE_FILE)).map_err(io("publish runtime promotion lease"))
}

fn create_lease_dir(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;

        let mut builder = fs::DirBuilder::new();
        builder.mode(0o700).create(path)
    }
    #[cfg(not(unix))]
    {
        fs::create_dir(path)
    }
}

fn read_record(path: &Path) -> Result<RuntimePromotionLeaseRecord> {
    let content =
        fs::read_to_string(path.join(LEASE_FILE)).map_err(io("read runtime promotion lease"))?;
    serde_json::from_str(&content).map_err(|e| {
        Error::validation_invalid_json(e, Some("parse runtime promotion lease".to_string()), None)
    })
}

fn read_record_for_acquisition(
    path: &Path,
) -> std::result::Result<RuntimePromotionLeaseRecord, LeaseRecordReadError> {
    let content = fs::read_to_string(path.join(LEASE_FILE)).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            LeaseRecordReadError::Disappeared(error)
        } else {
            LeaseRecordReadError::Failure(io("read runtime promotion lease")(error))
        }
    })?;
    serde_json::from_str(&content).map_err(|error| {
        LeaseRecordReadError::Failure(Error::validation_invalid_json(
            error,
            Some("parse runtime promotion lease".to_string()),
            None,
        ))
    })
}

fn subprocess_capability_from_env() -> Option<SubprocessLeaseCapability> {
    let encoded = std::env::var(SUBPROCESS_LEASE_ENV).ok()?;
    let payload = URL_SAFE_NO_PAD.decode(encoded).ok()?;
    serde_json::from_slice(&payload).ok()
}

fn authorizes_subprocess(
    held: &RuntimePromotionLeaseRecord,
    target: &str,
    generation: &str,
    capability: &SubprocessLeaseCapability,
) -> bool {
    authorizes_subprocess_with(
        held,
        target,
        generation,
        capability,
        |pid, linux_starttime_ticks, start_identity| {
            crate::process::process_identity_state_with_start_identity(
                pid,
                linux_starttime_ticks,
                start_identity,
            )
        },
    )
}

fn authorizes_subprocess_with(
    held: &RuntimePromotionLeaseRecord,
    target: &str,
    generation: &str,
    capability: &SubprocessLeaseCapability,
    inspect: impl FnOnce(
        u32,
        Option<u64>,
        Option<&crate::process::ProcessStartIdentity>,
    ) -> crate::process::ProcessIdentityState,
) -> bool {
    !held.capability.is_empty()
        && capability.owner_pid == held.pid
        && held.linux_starttime_ticks == capability.owner_linux_starttime_ticks
        && held.process_start_identity == capability.owner_process_start_identity
        && (held.linux_starttime_ticks.is_some() || held.process_start_identity.is_some())
        && capability.target == held.target
        && capability.generation == held.generation
        && capability.capability == held.capability
        && target == held.target
        && generation == held.generation
        && matches!(
            inspect(
                held.pid,
                held.linux_starttime_ticks,
                held.process_start_identity.as_ref(),
            ),
            crate::process::ProcessIdentityState::Live
        )
}

fn remember_local_capability(record: &RuntimePromotionLeaseRecord) -> Result<()> {
    let capability = SubprocessLeaseCapability {
        owner_pid: record.pid,
        owner_linux_starttime_ticks: record.linux_starttime_ticks,
        owner_process_start_identity: record.process_start_identity.clone(),
        target: record.target.clone(),
        generation: record.generation.clone(),
        capability: record.capability.clone(),
    };
    LOCAL_LEASE_CAPABILITIES.with(|capabilities| capabilities.borrow_mut().push(capability));
    Ok(())
}

fn forget_local_capability(pid: u32, target: &str, generation: &str, token: &str) {
    LOCAL_LEASE_CAPABILITIES.with(|capabilities| {
        let mut capabilities = capabilities.borrow_mut();
        if let Some(index) = capabilities.iter().rposition(|capability| {
            capability.owner_pid == pid
                && capability.target == target
                && capability.generation == generation
                && capability.capability == token
        }) {
            capabilities.remove(index);
        }
    });
}

fn authorizes_local_reentrancy(
    held: &RuntimePromotionLeaseRecord,
    target: &str,
    generation: &str,
) -> bool {
    LOCAL_LEASE_CAPABILITIES.with(|capabilities| {
        capabilities.borrow().iter().any(|capability| {
            capability.owner_pid == held.pid
                && capability.owner_linux_starttime_ticks == held.linux_starttime_ticks
                && capability.owner_process_start_identity == held.process_start_identity
                && capability.target == held.target
                && capability.generation == held.generation
                && capability.capability == held.capability
                && target == held.target
                && generation == held.generation
        })
    })
}

fn blocked_error(held: &RuntimePromotionLeaseRecord, reclaimable: bool) -> Error {
    let age = age_seconds(&held.started_at).unwrap_or(-1);
    let action = if reclaimable {
        "The holder is proven dead; retry the command to reclaim the lease automatically."
    } else {
        "Wait for the owner to finish, then follow with `homeboy self status`."
    };
    Error::new(
        ErrorCode::RuntimePromotionContended,
        format!(
            "runtime promotion is held by pid {} operation `{}` target `{}` for {}s",
            held.pid, held.operation, held.target, age
        ),
        serde_json::json!({
            "target": held.target,
            "holder_pid": held.pid,
            "holder_operation": held.operation,
            "holder_generation": held.generation,
            "holder_compatibility_key": held.compatibility_key,
            "reclaimable": reclaimable,
            "tried": [action, "Follow: `homeboy self doctor`"],
        }),
    )
}

fn contention_owner(error: &Error) -> Result<RuntimePromotionLeaseRecord> {
    let details = &error.details;
    let pid = details["holder_pid"].as_u64().ok_or_else(|| {
        Error::internal_unexpected("runtime promotion contention omitted holder pid")
    })? as u32;
    let string = |field: &str| {
        details[field].as_str().map(str::to_string).ok_or_else(|| {
            Error::internal_unexpected(format!("runtime promotion contention omitted {field}"))
        })
    };
    Ok(RuntimePromotionLeaseRecord {
        schema: "homeboy/runtime-promotion-lease/v2".to_string(),
        pid,
        operation: string("holder_operation")?,
        target: string("target")?,
        generation: string("holder_generation")?,
        compatibility_key: details["holder_compatibility_key"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        started_at: String::new(),
        linux_starttime_ticks: None,
        process_start_identity: None,
        heartbeat_at: String::new(),
        expires_at: String::new(),
        capability: String::new(),
    })
}

fn is_compatible_owner(
    owner: &RuntimePromotionLeaseRecord,
    target: &str,
    generation: &str,
    compatibility_key: &str,
) -> bool {
    owner.target == target
        && owner.generation == generation
        && (compatibility_key.is_empty()
            || (!owner.compatibility_key.is_empty()
                && owner.compatibility_key == compatibility_key))
}

fn wait_timeout_error(owner: &RuntimePromotionLeaseRecord, timeout: Duration) -> Error {
    Error::new(
        ErrorCode::RuntimePromotionWaitTimeout,
        format!(
            "runtime promotion wait timed out after {}s behind pid {} operation `{}` target `{}`",
            timeout.as_secs(),
            owner.pid,
            owner.operation,
            owner.target
        ),
        serde_json::json!({
            "schema": "homeboy/runtime-promotion-admission/v1",
            "queue_state": "timed_out_waiting_for_compatible_owner",
            "resource_class": "runtime_promotion",
            "state": "busy",
            "wait_timeout_seconds": timeout.as_secs(),
            "target": owner.target,
            "holder_pid": owner.pid,
            "holder_operation": owner.operation,
            "holder_generation": owner.generation,
            "holder_compatibility_key": owner.compatibility_key,
            "tried": ["The compatible promotion owner did not finish before the admission deadline.", "Retry the same command after the owner completes or inspect its runtime-promotion lease."],
        }),
    )
}

pub fn is_contention_error(error: &Error) -> bool {
    error.code == ErrorCode::RuntimePromotionContended
}

fn reclaimable(record: &RuntimePromotionLeaseRecord) -> bool {
    reclaimable_with(record, |pid, linux_starttime_ticks, start_identity| {
        crate::process::process_identity_state_with_start_identity(
            pid,
            linux_starttime_ticks,
            start_identity,
        )
    })
}

fn reclaimable_with(
    record: &RuntimePromotionLeaseRecord,
    inspect: impl FnOnce(
        u32,
        Option<u64>,
        Option<&crate::process::ProcessStartIdentity>,
    ) -> crate::process::ProcessIdentityState,
) -> bool {
    matches!(
        inspect(
            record.pid,
            record.linux_starttime_ticks,
            record.process_start_identity.as_ref(),
        ),
        crate::process::ProcessIdentityState::Dead
            | crate::process::ProcessIdentityState::IdentityMismatch
    )
}

fn prune_pins(root: &Path) -> Result<()> {
    for entry in fs::read_dir(root).map_err(io("read runtime generation pins"))? {
        let path = entry.map_err(io("read runtime generation pin"))?.path();
        let content = match fs::read_to_string(&path) {
            Ok(value) => value,
            Err(_) => continue,
        };
        let Ok(pin) = serde_json::from_str::<RuntimeGenerationPin>(&content) else {
            continue;
        };
        if !crate::process::pid_is_running(pin.pid)
            || age_seconds(&pin.started_at).is_some_and(|age| age >= DEFAULT_TTL.as_secs() as i64)
        {
            let _ = fs::remove_file(path);
        }
    }
    Ok(())
}

/// A pending promotion holds exclusive admission while existing pins drain.
/// Cooks take the shared side before publishing a pin, so once this scan finds
/// no foreign pin the promotion is ahead of all later cook admission.
fn wait_for_foreign_generation_pins(
    root: &Path,
    pid: u32,
    subprocess_capability: Option<&SubprocessLeaseCapability>,
) -> Result<()> {
    let pins = root.join(PIN_DIR);
    if !pins.exists() {
        return Ok(());
    }
    loop {
        prune_pins(&pins)?;
        let foreign_pin = fs::read_dir(&pins)
            .map_err(io("read runtime generation pins"))?
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter_map(|path| fs::read_to_string(path).ok())
            .filter_map(|content| serde_json::from_str::<RuntimeGenerationPin>(&content).ok())
            .any(|pin| {
                pin.pid != pid
                    && !subprocess_capability
                        .is_some_and(|capability| capability.owner_pid == pin.pid)
            });
        if !foreign_pin {
            return Ok(());
        }
        std::thread::sleep(PIN_DRAIN_POLL);
    }
}

fn current_generation() -> String {
    build_identity::current().display
}
fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}
fn expiry() -> String {
    (chrono::Utc::now() + chrono::Duration::from_std(DEFAULT_TTL).expect("lease TTL fits chrono"))
        .to_rfc3339()
}
fn age_seconds(started: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(started)
        .ok()
        .map(|time| chrono::Utc::now().signed_duration_since(time).num_seconds())
}
fn io(context: &'static str) -> impl FnOnce(std::io::Error) -> Error {
    move |error| Error::internal_io(error.to_string(), Some(context.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_owner_is_bounded_by_liveness_or_expiry() {
        let dead = RuntimePromotionLeaseRecord {
            schema: "v1".to_string(),
            pid: u32::MAX,
            operation: "upgrade".to_string(),
            target: "main".to_string(),
            generation: "old".to_string(),
            compatibility_key: String::new(),
            started_at: now(),
            linux_starttime_ticks: None,
            process_start_identity: None,
            heartbeat_at: now(),
            expires_at: expiry(),
            capability: "capability".to_string(),
        };
        assert!(reclaimable_with(&dead, |_, _, _| {
            crate::process::ProcessIdentityState::Dead
        }));
    }

    #[test]
    fn reused_pid_is_reclaimable_but_live_or_unverifiable_owner_remains_protected() {
        let record = lease_record();
        assert!(reclaimable_with(&record, |_, _, _| {
            crate::process::ProcessIdentityState::IdentityMismatch
        }));
        assert!(!reclaimable_with(&record, |_, _, _| {
            crate::process::ProcessIdentityState::Live
        }));
        assert!(!reclaimable_with(&record, |_, _, _| {
            crate::process::ProcessIdentityState::Unverifiable
        }));
    }

    #[test]
    fn panic_unwinds_the_primary_lease() {
        crate::test_support::with_isolated_home(|_| {
            let _ = std::panic::catch_unwind(|| {
                let _lease = acquire("panic owner", "lab").expect("acquire lease");
                panic!("interrupted mutation");
            });
            acquire("recovery", "lab").expect("drop releases a panicking owner lease");
        });
    }

    #[test]
    fn killed_owner_is_reclaimed_automatically() {
        crate::test_support::with_isolated_home(|_| {
            let mut owner = compatible_wait_owner();
            owner.kill().expect("kill owner");
            owner.wait().expect("reap owner");
            acquire("recovery", "lab").expect("dead owner is reclaimed without a manual takeover");
        });
    }
    #[test]
    fn blocked_diagnostic_names_owner_operation_target_and_followup() {
        let held = RuntimePromotionLeaseRecord {
            schema: "v1".to_string(),
            pid: 42,
            operation: "runner refresh".to_string(),
            target: "lab".to_string(),
            generation: "old".to_string(),
            compatibility_key: String::new(),
            started_at: now(),
            linux_starttime_ticks: None,
            process_start_identity: None,
            heartbeat_at: now(),
            expires_at: expiry(),
            capability: "capability".to_string(),
        };
        let error = blocked_error(&held, false);
        assert_eq!(error.code, ErrorCode::RuntimePromotionContended);
        assert!(error.message.contains("pid 42"));
        assert!(error.message.contains("runner refresh"));
        assert!(error.message.contains("lab"));
        assert!(format!("{:?}", error).contains("self status"));
    }

    #[test]
    fn compatible_same_target_and_generation_contenders_converge_after_owner_release() {
        crate::test_support::with_isolated_home(|_| {
            let owner = acquire_waiting_for_compatible_key(
                "owner",
                "lab",
                "candidate-a",
                Duration::from_secs(1),
                |_| unreachable!("uncontended owner does not queue"),
            )
            .expect("owner acquires lease");
            let (queued, queued_result) = std::sync::mpsc::channel();
            let contenders = (0..3)
                .map(|_| {
                    let queued = queued.clone();
                    std::thread::spawn(move || {
                        acquire_waiting_for_compatible_key(
                            "contender",
                            "lab",
                            "candidate-a",
                            Duration::from_secs(1),
                            |event| {
                                queued
                                    .send((event.owner_pid, event.target, event.owner_generation))
                                    .expect("report queued owner")
                            },
                        )
                        .map(drop)
                    })
                })
                .collect::<Vec<_>>();
            drop(queued);

            for _ in 0..3 {
                assert_eq!(
                    queued_result
                        .recv_timeout(Duration::from_secs(1))
                        .expect("contender reports the current owner"),
                    (std::process::id(), "lab".to_string(), current_generation(),)
                );
            }
            drop(owner);

            for contender in contenders {
                contender
                    .join()
                    .expect("contender exits")
                    .expect("compatible contender acquires after deterministic handoff");
            }
        });
    }

    #[test]
    fn compatible_wait_timeout_leaves_no_queue_state() {
        crate::test_support::with_isolated_home(|_| {
            let mut owner = compatible_wait_owner();
            let events = std::sync::Mutex::new(Vec::new());
            let error = acquire_waiting_for_compatible(
                "cancelled contender",
                "lab",
                Duration::from_millis(25),
                |event| events.lock().expect("collect admission events").push(event),
            )
            .expect_err("bounded contender wait times out");
            assert_eq!(error.code, ErrorCode::RuntimePromotionWaitTimeout);
            assert_eq!(
                error.details["queue_state"],
                "timed_out_waiting_for_compatible_owner"
            );
            assert_eq!(error.details["resource_class"], "runtime_promotion");
            assert_eq!(error.details["state"], "busy");
            let events = events
                .into_inner()
                .expect("admission events are not poisoned");
            assert!(
                events.len() >= 2,
                "the queued wait must emit an immediate event and a heartbeat"
            );
            assert!(events.iter().all(|event| {
                event.schema == "homeboy/runtime-promotion-admission/v1"
                    && event.state == "queued"
                    && event.resource_class == "runtime_promotion"
                    && event.target == "lab"
                    && event.owner_pid == owner.id()
                    && event.wait_timeout_ms == 25
            }));
            assert!(events
                .windows(2)
                .all(|events| events[1].waited_ms >= events[0].waited_ms));

            owner.wait().expect("owner exits");
            acquire("later contender", "lab")
                .expect("timed-out contender did not leave a queue entry behind");
        });
    }

    #[test]
    fn incompatible_contender_remains_fail_closed() {
        crate::test_support::with_isolated_home(|_| {
            let _owner = acquire("owner operation", "lab").expect("owner acquires lease");
            let error = acquire_waiting_for_compatible(
                "other target",
                "other-lab",
                Duration::from_secs(1),
                |_| panic!("incompatible contender must not join the queue"),
            )
            .expect_err("different promotion target remains serialized");
            assert_eq!(error.code, ErrorCode::RuntimePromotionContended);
            assert_eq!(error.details["holder_pid"], std::process::id());
            assert_eq!(error.details["holder_operation"], "owner operation");
            assert_eq!(error.details["target"], "lab");
            assert_eq!(error.details["holder_generation"], current_generation());
        });
    }

    #[test]
    fn compatibility_is_asymmetric_for_unkeyed_and_keyed_owners() {
        let owner = lease_record();
        assert!(is_compatible_owner(
            &owner,
            "lab",
            "generation-a",
            "candidate-a"
        ));
        assert!(!is_compatible_owner(
            &owner,
            "other-lab",
            "generation-a",
            "candidate-a"
        ));
        assert!(!is_compatible_owner(
            &owner,
            "lab",
            "generation-b",
            "candidate-a"
        ));
        assert!(!is_compatible_owner(
            &owner,
            "lab",
            "generation-a",
            "candidate-b"
        ));
        let mut legacy_owner = owner.clone();
        legacy_owner.compatibility_key.clear();
        assert!(is_compatible_owner(&owner, "lab", "generation-a", ""));
        assert!(!is_compatible_owner(
            &legacy_owner,
            "lab",
            "generation-a",
            "candidate-a"
        ));
    }

    fn compatible_wait_owner() -> std::process::Child {
        let executable = std::env::current_exe().expect("resolve test executable");
        let child = Command::new(executable)
            .args([
                "--ignored",
                "--exact",
                "runtime_promotion::tests::compatible_wait_owner_child",
            ])
            .spawn()
            .expect("start compatible wait owner");
        let lock = paths::runtime_promotion_dir()
            .expect("runtime promotion directory")
            .join(LEASE_DIR);
        for _ in 0..50 {
            if lock.exists() {
                return child;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("compatible wait owner did not acquire the lease");
    }

    #[test]
    #[ignore = "invoked by compatible promotion wait tests"]
    fn compatible_wait_owner_child() {
        let _lease = acquire("child owner", "lab").expect("child acquires lease");
        std::thread::sleep(Duration::from_millis(250));
    }

    fn lease_record() -> RuntimePromotionLeaseRecord {
        RuntimePromotionLeaseRecord {
            schema: "homeboy/runtime-promotion-lease/v2".to_string(),
            pid: 42,
            operation: "runner refresh".to_string(),
            target: "lab".to_string(),
            generation: "generation-a".to_string(),
            compatibility_key: "candidate-a".to_string(),
            started_at: now(),
            linux_starttime_ticks: Some(42),
            process_start_identity: Some(crate::process::ProcessStartIdentity::Linux {
                starttime_ticks: 42,
            }),
            heartbeat_at: now(),
            expires_at: expiry(),
            capability: "unforgeable-capability".to_string(),
        }
    }

    fn capability(record: &RuntimePromotionLeaseRecord) -> SubprocessLeaseCapability {
        SubprocessLeaseCapability {
            owner_pid: record.pid,
            owner_linux_starttime_ticks: record.linux_starttime_ticks,
            owner_process_start_identity: record.process_start_identity.clone(),
            target: record.target.clone(),
            generation: record.generation.clone(),
            capability: record.capability.clone(),
        }
    }

    #[test]
    fn acquire_retries_once_when_the_observed_lease_disappears() {
        let mut create_calls = 0;
        let mut read_calls = 0;

        let result = acquire_lease_dir_with_retry(
            || {
                create_calls += 1;
                if create_calls == 1 {
                    Err(std::io::Error::from(std::io::ErrorKind::AlreadyExists))
                } else {
                    Ok(())
                }
            },
            || {
                read_calls += 1;
                Err(LeaseRecordReadError::Disappeared(std::io::Error::from(
                    std::io::ErrorKind::NotFound,
                )))
            },
        )
        .expect("the second acquisition succeeds after the former owner removes its lease");

        assert!(result.is_none());
        assert_eq!(create_calls, 2);
        assert_eq!(read_calls, 1);
    }

    #[test]
    fn acquire_does_not_retry_a_malformed_or_unreadable_lease() {
        let mut create_calls = 0;
        let mut read_calls = 0;

        let error = acquire_lease_dir_with_retry(
            || {
                create_calls += 1;
                Err(std::io::Error::from(std::io::ErrorKind::AlreadyExists))
            },
            || {
                read_calls += 1;
                Err(LeaseRecordReadError::Failure(Error::internal_io(
                    "invalid lease record".to_string(),
                    Some("parse runtime promotion lease".to_string()),
                )))
            },
        )
        .expect_err("a malformed or unreadable lease remains an error");

        assert_eq!(error.code, ErrorCode::InternalIoError);
        assert_eq!(create_calls, 1);
        assert_eq!(read_calls, 1);
    }

    #[test]
    fn duplicate_cook_pins_keep_the_remaining_guard_live() {
        crate::test_support::with_isolated_home(|_| {
            let first = pin_cook_generation("duplicate-cook").expect("first cook pin");
            let second = pin_cook_generation("duplicate-cook").expect("second cook pin");
            let pins = paths::runtime_promotion_dir()
                .expect("runtime promotion directory")
                .join(PIN_DIR);
            assert_eq!(
                fs::read_dir(&pins).expect("list pins").count(),
                2,
                "each concurrent cook guard owns a distinct pin"
            );

            drop(first);
            assert_eq!(
                fs::read_dir(&pins).expect("list remaining pin").count(),
                1,
                "dropping one duplicate cook guard must retain the other pin"
            );
            assert!(second.path.exists(), "the second guard still owns its pin");
        });
    }

    #[test]
    fn generation_rotation_retains_writer_serialization_without_waiting_for_foreign_cooks() {
        crate::test_support::with_isolated_home(|_| {
            let mut foreign_owner = Command::new("sleep")
                .arg("30")
                .spawn()
                .expect("start live foreign pin owner");
            let pins = paths::runtime_promotion_dir()
                .expect("runtime promotion directory")
                .join(PIN_DIR);
            fs::create_dir_all(&pins).expect("create pin directory");
            fs::write(
                pins.join("foreign-cook.json"),
                serde_json::to_vec_pretty(&RuntimeGenerationPin {
                    pid: foreign_owner.id(),
                    cook_id: "foreign-cook".to_string(),
                    generation: "existing-generation".to_string(),
                    started_at: now(),
                })
                .expect("serialize foreign pin"),
            )
            .expect("write foreign pin");

            let rotation = acquire_for_generation_rotation("runner rotation", "lab")
                .expect("generation-preserving rotation can acquire the writer lease");
            let concurrent = acquire_for_generation_rotation("other rotation", "other")
                .expect_err("the writer lease still serializes generation rotations");
            assert_eq!(concurrent.code, ErrorCode::RuntimePromotionContended);
            drop(rotation);
            foreign_owner.kill().expect("stop foreign pin owner");
            foreign_owner.wait().expect("reap foreign pin owner");
        });
    }

    #[test]
    fn pending_promotion_drains_existing_pins_before_admitting_new_cooks() {
        crate::test_support::with_isolated_home(|_| {
            let mut existing_owner = Command::new("sleep")
                .arg("30")
                .spawn()
                .expect("start existing cook owner");
            let pins = paths::runtime_promotion_dir()
                .expect("runtime promotion directory")
                .join(PIN_DIR);
            fs::create_dir_all(&pins).expect("create pin directory");
            fs::write(
                pins.join("existing-cook.json"),
                serde_json::to_vec_pretty(&RuntimeGenerationPin {
                    pid: existing_owner.id(),
                    cook_id: "existing-attempt".to_string(),
                    generation: "existing-generation".to_string(),
                    started_at: now(),
                })
                .expect("serialize existing pin"),
            )
            .expect("write existing pin");

            let (promotion_ready, promotion_ready_result) = std::sync::mpsc::channel();
            let (release_promotion, release_promotion_result) = std::sync::mpsc::channel();
            let promotion = std::thread::spawn(move || {
                let lease = acquire("controller replacement", "controller")
                    .expect("promotion waits for existing pin");
                promotion_ready
                    .send(())
                    .expect("report promotion admission");
                release_promotion_result
                    .recv()
                    .expect("wait to release promotion");
                drop(lease);
            });

            let admission = paths::runtime_promotion_dir()
                .expect("runtime promotion directory")
                .join(ADMISSION_LOCK_FILE);
            for _ in 0..40 {
                if admission.exists() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            assert!(
                admission.exists(),
                "promotion reserves cook admission before draining"
            );

            let (new_cook_admitted, new_cook_admitted_result) = std::sync::mpsc::channel();
            let new_cooks = (0..3)
                .map(|attempt| {
                    let new_cook_admitted = new_cook_admitted.clone();
                    std::thread::spawn(move || {
                        let _pin = pin_cook_generation(&format!("new-attempt-{attempt}"))
                            .expect("queue new cook pin");
                        new_cook_admitted
                            .send(())
                            .expect("report new cook admission");
                    })
                })
                .collect::<Vec<_>>();
            drop(new_cook_admitted);
            assert!(
                new_cook_admitted_result
                    .recv_timeout(Duration::from_millis(250))
                    .is_err(),
                "continuous new cook admission stays queued behind the pending promotion"
            );

            existing_owner.kill().expect("stop existing cook owner");
            existing_owner.wait().expect("reap existing cook owner");
            promotion_ready_result
                .recv_timeout(Duration::from_secs(5))
                .expect("promotion proceeds after old-generation work drains");
            assert!(
                new_cook_admitted_result.try_recv().is_err(),
                "promotion owns admission before the queued cook"
            );

            release_promotion
                .send(())
                .expect("release promotion admission");
            promotion.join().expect("promotion exits");
            for _ in 0..3 {
                new_cook_admitted_result
                    .recv_timeout(Duration::from_secs(5))
                    .expect("queued cook admits after promotion");
            }
            for new_cook in new_cooks {
                new_cook.join().expect("new cook exits");
            }
        });
    }

    #[test]
    fn pinned_handoff_blocks_a_concurrent_runner_binary_promotion() {
        crate::test_support::with_isolated_home(|_| {
            let mut handoff = pinned_handoff_owner();
            let pins = paths::runtime_promotion_dir()
                .expect("runtime promotion directory")
                .join(PIN_DIR);
            for _ in 0..50 {
                if pins.exists() && fs::read_dir(&pins).expect("list pins").count() == 1 {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            assert!(pins.exists(), "handoff pin was published");

            let (promoted, promoted_result) = std::sync::mpsc::channel();
            let promotion = std::thread::spawn(move || {
                let _lease = acquire("runner binary promotion", "runner-a")
                    .expect("promotion acquires after handoff drains");
                promoted.send(()).expect("report promotion");
            });
            assert!(
                promoted_result
                    .recv_timeout(Duration::from_millis(100))
                    .is_err(),
                "runner promotion must not replace a generation while the handoff is pinned"
            );

            handoff.wait().expect("handoff exits and releases its pin");
            promoted_result
                .recv_timeout(Duration::from_secs(5))
                .expect("promotion proceeds after pin release");
            promotion.join().expect("promotion exits");
        });
    }

    fn pinned_handoff_owner() -> std::process::Child {
        let executable = std::env::current_exe().expect("resolve test executable");
        Command::new(executable)
            .args([
                "--ignored",
                "--exact",
                "runtime_promotion::tests::pinned_handoff_owner_child",
            ])
            .spawn()
            .expect("start pinned handoff owner")
    }

    #[test]
    #[ignore = "invoked by pinned_handoff_blocks_a_concurrent_runner_binary_promotion"]
    fn pinned_handoff_owner_child() {
        let _pin = pin_cook_generation("handoff-attempt").expect("pin handoff generation");
        std::thread::sleep(Duration::from_millis(350));
    }

    #[test]
    fn authorized_child_reenters_only_its_parent_transaction() {
        crate::test_support::with_isolated_home(|_| {
            let lease = acquire("parent", "lab").expect("parent acquires lease");
            let executable = std::env::current_exe().expect("resolve test executable");
            let mut child = Command::new(executable);
            child.args([
                "--ignored",
                "--exact",
                "core::runtime_promotion::tests::authorized_child_process_acquires_lease",
            ]);
            lease.authorize_subprocess(&mut child);
            assert!(child.status().expect("run authorized child").success());
        });
    }

    #[test]
    #[ignore = "invoked by authorized_child_reenters_only_its_parent_transaction"]
    fn authorized_child_process_acquires_lease() {
        acquire("child", "lab").expect("authorized child reenters parent lease");
    }

    #[test]
    fn unrelated_process_without_capability_is_denied() {
        let held = lease_record();
        assert!(!authorizes_subprocess(
            &held,
            "lab",
            "generation-a",
            &SubprocessLeaseCapability {
                owner_pid: 99,
                owner_linux_starttime_ticks: Some(42),
                owner_process_start_identity: Some(crate::process::ProcessStartIdentity::Linux {
                    starttime_ticks: 42,
                }),
                target: "lab".to_string(),
                generation: "generation-a".to_string(),
                capability: "unforgeable-capability".to_string(),
            }
        ));
        crate::test_support::with_isolated_home(|_| {
            let _lease = acquire("parent", "lab").expect("parent acquires lease");
            let executable = std::env::current_exe().expect("resolve test executable");
            let status = Command::new(executable)
                .args([
                    "--ignored",
                    "--exact",
                    "core::runtime_promotion::tests::unrelated_child_process_is_denied",
                ])
                .env_remove(SUBPROCESS_LEASE_ENV)
                .status()
                .expect("run unrelated child");
            assert!(status.success());
        });
    }

    #[test]
    #[ignore = "invoked by unrelated_process_without_capability_is_denied"]
    fn unrelated_child_process_is_denied() {
        assert!(acquire("child", "lab").is_err());
    }

    #[test]
    fn subprocess_capability_rejects_wrong_token_target_and_generation() {
        let held = lease_record();
        let mut wrong_token = capability(&held);
        wrong_token.capability = "wrong".to_string();
        assert!(!authorizes_subprocess(
            &held,
            "lab",
            "generation-a",
            &wrong_token
        ));
        assert!(!authorizes_subprocess(
            &held,
            "other",
            "generation-a",
            &capability(&held)
        ));
        assert!(!authorizes_subprocess(
            &held,
            "lab",
            "generation-b",
            &capability(&held)
        ));
    }

    #[test]
    fn subprocess_capability_rejects_a_process_start_identity_mismatch() {
        let held = lease_record();
        let mut mismatched = capability(&held);
        mismatched.owner_process_start_identity =
            Some(crate::process::ProcessStartIdentity::Macos {
                start_seconds: 1,
                start_microseconds: 2,
            });

        assert!(!authorizes_subprocess_with(
            &held,
            "lab",
            "generation-a",
            &mismatched,
            |_, _, _| panic!("mismatched capability must be rejected before process inspection"),
        ));
    }

    #[test]
    fn exact_capability_and_process_identity_are_required_for_reentrancy() {
        let held = lease_record();
        let capability = capability(&held);
        assert!(authorizes_subprocess_with(
            &held,
            "lab",
            "generation-a",
            &capability,
            |_, _, _| crate::process::ProcessIdentityState::Live,
        ));

        let mut forged = capability.clone();
        forged.capability = "forged".to_string();
        assert!(!authorizes_subprocess_with(
            &held,
            "lab",
            "generation-a",
            &forged,
            |_, _, _| crate::process::ProcessIdentityState::Live,
        ));
        assert!(!authorizes_subprocess_with(
            &held,
            "lab",
            "generation-a",
            &capability,
            |_, _, _| crate::process::ProcessIdentityState::IdentityMismatch,
        ));
    }

    #[test]
    fn legacy_lease_without_start_identity_cannot_grant_nested_ownership() {
        let mut held = lease_record();
        held.linux_starttime_ticks = None;
        held.process_start_identity = None;
        let capability = SubprocessLeaseCapability {
            owner_pid: held.pid,
            owner_linux_starttime_ticks: None,
            owner_process_start_identity: None,
            target: held.target.clone(),
            generation: held.generation.clone(),
            capability: held.capability.clone(),
        };
        assert!(!authorizes_subprocess_with(
            &held,
            "lab",
            "generation-a",
            &capability,
            |_, _, _| crate::process::ProcessIdentityState::Live,
        ));
    }

    #[test]
    fn v2_linux_lease_and_capability_remain_compatible() {
        let record: RuntimePromotionLeaseRecord = serde_json::from_value(serde_json::json!({
            "schema": "homeboy/runtime-promotion-lease/v2",
            "pid": 42,
            "operation": "runner refresh",
            "target": "lab",
            "generation": "generation-a",
            "started_at": "2026-01-01T00:00:00Z",
            "linux_starttime_ticks": 42,
            "heartbeat_at": "2026-01-01T00:00:00Z",
            "expires_at": "2026-01-01T00:30:00Z",
            "capability": "unforgeable-capability"
        }))
        .expect("v2 record without the additive identity field deserializes");
        let capability: SubprocessLeaseCapability = serde_json::from_value(serde_json::json!({
            "owner_pid": 42,
            "owner_linux_starttime_ticks": 42,
            "target": "lab",
            "generation": "generation-a",
            "capability": "unforgeable-capability"
        }))
        .expect("v2 capability without the additive identity field deserializes");

        assert!(authorizes_subprocess_with(
            &record,
            "lab",
            "generation-a",
            &capability,
            |_, _, _| crate::process::ProcessIdentityState::Live,
        ));
    }

    #[test]
    fn nested_lease_requires_an_exact_local_capability() {
        crate::test_support::with_isolated_home(|_| {
            let outer = acquire("outer", "lab").expect("outer acquires");
            let inner = acquire("inner", "lab").expect("exact local capability reenters");
            assert!(!inner.primary, "nested lease must not own cleanup");
            drop(inner);
            drop(outer);
        });
    }

    #[test]
    fn keyed_owner_allows_legacy_reentrancy_without_releasing_its_transaction() {
        crate::test_support::with_isolated_home(|_| {
            let outer = acquire_waiting_for_compatible_key(
                "keyed owner",
                "lab",
                "candidate-a",
                Duration::from_secs(1),
                |_| unreachable!("uncontended owner does not queue"),
            )
            .expect("keyed owner acquires lease");
            let inner = acquire("legacy nested operation", "lab")
                .expect("the owner's exact local capability permits legacy reentrancy");

            assert!(!inner.primary, "nested lease must not own cleanup");
            assert_eq!(inner.compatibility_key, "candidate-a");
            drop(inner);

            let path = paths::runtime_promotion_dir()
                .expect("runtime promotion directory")
                .join(LEASE_DIR);
            let record = read_record(&path).expect("primary lease remains published");
            assert_eq!(record.compatibility_key, "candidate-a");
            assert_eq!(record.capability, outer.capability);

            let contention = std::thread::spawn(|| acquire("independent operation", "lab"))
                .join()
                .expect("independent contender exits")
                .expect_err("another thread cannot use the owner's local capability");
            assert_eq!(contention.code, ErrorCode::RuntimePromotionContended);

            drop(outer);
            acquire("later operation", "lab")
                .expect("dropping the primary releases the transaction after nested cleanup");
        });
    }

    #[test]
    fn primary_cleanup_keeps_a_replaced_lease() {
        let temporary = tempfile::tempdir().expect("temporary lease directory");
        let path = temporary.path().join(LEASE_DIR);
        fs::create_dir(&path).expect("create lease directory");
        let mut record = lease_record();
        write_record(&path, &record).expect("write initial lease");
        let lease = RuntimePromotionLease {
            path: path.clone(),
            primary: true,
            generation: record.generation.clone(),
            target: record.target.clone(),
            compatibility_key: record.compatibility_key.clone(),
            owner_pid: record.pid,
            capability: record.capability.clone(),
            admission_lock: None,
        };
        record.capability = "replacement-capability".to_string();
        write_record(&path, &record).expect("replace lease record");
        drop(lease);
        assert!(path.exists(), "a former owner cannot remove a replacement");
    }
}
