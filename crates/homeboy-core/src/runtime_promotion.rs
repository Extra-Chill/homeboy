//! Machine-global serialization for operations that replace or select Homeboy
//! binaries. The directory-create guard follows the established rig lease lock
//! convention, while the JSON record makes a blocked writer actionable.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use fs4::fs_std::FileExt;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

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
const ACQUIRE_DISAPPEARED_LEASE_RETRIES: usize = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimePromotionLeaseRecord {
    pub schema: String,
    pub pid: u32,
    pub operation: String,
    pub target: String,
    pub generation: String,
    pub started_at: String,
    /// A random capability is required when the transaction crosses a process
    /// boundary. The promotion directory is already local-user state.
    #[serde(default)]
    pub capability: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SubprocessLeaseCapability {
    owner_pid: u32,
    target: String,
    generation: String,
    capability: String,
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
            // Never remove a lease that an explicit takeover or another owner
            // has replaced since this guard was acquired.
            if read_record(&self.path).is_ok_and(|record| {
                record.pid == self.owner_pid
                    && record.target == self.target
                    && record.generation == self.generation
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
    acquire_with_pin_policy(operation, target.into(), ForeignPinPolicy::Block)
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
    acquire_with_pin_policy(operation, target.into(), ForeignPinPolicy::Allow)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ForeignPinPolicy {
    Block,
    Allow,
}

fn acquire_with_pin_policy(
    operation: &str,
    target: String,
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
            write_record(
                &path,
                &RuntimePromotionLeaseRecord {
                    schema: "homeboy/runtime-promotion-lease/v2".to_string(),
                    pid,
                    operation: operation.to_string(),
                    target: target.clone(),
                    generation: generation.clone(),
                    started_at: now(),
                    capability: capability.clone(),
                },
            )?;
            RuntimePromotionLease {
                path,
                primary: true,
                generation,
                target,
                owner_pid: pid,
                capability,
                admission_lock: None,
            }
        }
        Some(held) => {
            let same_transaction = held.target == target && held.generation == generation;
            if held.pid == pid && same_transaction {
                return Ok(RuntimePromotionLease {
                    path,
                    primary: false,
                    generation,
                    target,
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
                    owner_pid: held.pid,
                    capability: held.capability,
                    admission_lock: None,
                });
            }
            if reclaimable(&held) {
                return Err(blocked_error(&held, true));
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
/// can remove its directory after our create attempt observes it, so retry the
/// create exactly once when the record is gone before it can be read.
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

/// Archive, rather than delete, a proven dead or expired promotion lease. This
/// is intentionally a separate operator action: automatic acquisition never
/// steals a writer merely because its record looks old.
pub fn takeover_stale_lease() -> Result<RuntimePromotionTakeover> {
    let root = paths::runtime_promotion_dir()?;
    let path = root.join(LEASE_DIR);
    let previous = read_record(&path)?;
    if !reclaimable(&previous) {
        return Err(blocked_error(&previous, false));
    }
    let archived = root.join(format!(
        "promotion.stale.{}.lock",
        chrono::Utc::now().timestamp_millis()
    ));
    fs::rename(&path, &archived).map_err(io("archive stale runtime promotion lease"))?;
    Ok(RuntimePromotionTakeover {
        previous,
        archived_path: archived.display().to_string(),
    })
}

fn write_record(path: &Path, record: &RuntimePromotionLeaseRecord) -> Result<()> {
    let payload =
        serde_json::to_vec_pretty(record).map_err(|e| Error::internal_json(e.to_string(), None))?;
    fs::write(path.join(LEASE_FILE), payload).map_err(io("write runtime promotion lease"))
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
    !held.capability.is_empty()
        && capability.owner_pid == held.pid
        && capability.target == held.target
        && capability.generation == held.generation
        && capability.capability == held.capability
        && target == held.target
        && generation == held.generation
}

fn blocked_error(held: &RuntimePromotionLeaseRecord, reclaimable: bool) -> Error {
    let age = age_seconds(&held.started_at).unwrap_or(-1);
    let action = if reclaimable {
        "The holder is dead or expired; run `homeboy runtime promotion-takeover` to record an explicit takeover."
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
            "reclaimable": reclaimable,
            "tried": [action, "Follow: `homeboy self doctor`"],
        }),
    )
}

pub fn is_contention_error(error: &Error) -> bool {
    error.code == ErrorCode::RuntimePromotionContended
}

fn reclaimable(record: &RuntimePromotionLeaseRecord) -> bool {
    !crate::process::pid_is_running(record.pid)
        || age_seconds(&record.started_at).is_some_and(|age| age >= DEFAULT_TTL.as_secs() as i64)
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
            started_at: now(),
            capability: "capability".to_string(),
        };
        assert!(reclaimable(&dead));
    }
    #[test]
    fn blocked_diagnostic_names_owner_operation_target_and_followup() {
        let held = RuntimePromotionLeaseRecord {
            schema: "v1".to_string(),
            pid: 42,
            operation: "runner refresh".to_string(),
            target: "lab".to_string(),
            generation: "old".to_string(),
            started_at: now(),
            capability: "capability".to_string(),
        };
        let error = blocked_error(&held, false);
        assert_eq!(error.code, ErrorCode::RuntimePromotionContended);
        assert!(error.message.contains("pid 42"));
        assert!(error.message.contains("runner refresh"));
        assert!(error.message.contains("lab"));
        assert!(format!("{:?}", error).contains("self status"));
    }

    fn lease_record() -> RuntimePromotionLeaseRecord {
        RuntimePromotionLeaseRecord {
            schema: "homeboy/runtime-promotion-lease/v2".to_string(),
            pid: 42,
            operation: "runner refresh".to_string(),
            target: "lab".to_string(),
            generation: "generation-a".to_string(),
            started_at: now(),
            capability: "unforgeable-capability".to_string(),
        }
    }

    fn capability(record: &RuntimePromotionLeaseRecord) -> SubprocessLeaseCapability {
        SubprocessLeaseCapability {
            owner_pid: record.pid,
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
