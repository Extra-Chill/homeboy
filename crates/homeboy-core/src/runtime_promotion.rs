//! Machine-global serialization for operations that replace or select Homeboy
//! binaries. The directory-create guard follows the established rig lease lock
//! convention, while the JSON record makes a blocked writer actionable.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
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
const PIN_DIR: &str = "pins";
const DEFAULT_TTL: Duration = Duration::from_secs(30 * 60);
const SUBPROCESS_LEASE_ENV: &str = "HOMEBOY_RUNTIME_PROMOTION_LEASE";

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
    let target = target.into();
    let root = paths::runtime_promotion_dir()?;
    fs::create_dir_all(&root).map_err(io("create runtime promotion directory"))?;
    let path = root.join(LEASE_DIR);
    let pid = std::process::id();
    let generation = current_generation();
    let subprocess_capability = subprocess_capability_from_env();

    match create_lease_dir(&path) {
        Ok(()) => {
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
            Ok(RuntimePromotionLease {
                path,
                primary: true,
                generation,
                target,
                owner_pid: pid,
                capability,
            })
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let held = read_record(&path)?;
            let same_transaction = held.target == target && held.generation == generation;
            if held.pid == pid && same_transaction {
                return Ok(RuntimePromotionLease {
                    path,
                    primary: false,
                    generation,
                    target,
                    owner_pid: held.pid,
                    capability: held.capability,
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
                });
            }
            if reclaimable(&held) {
                return Err(blocked_error(&held, true));
            }
            Err(blocked_error(&held, false))
        }
        Err(error) => Err(Error::internal_io(
            error.to_string(),
            Some("acquire runtime promotion lease".to_string()),
        )),
    }
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
    let root = paths::runtime_promotion_dir()?.join(PIN_DIR);
    fs::create_dir_all(&root).map_err(io("create runtime generation pin directory"))?;
    prune_pins(&root)?;
    let pid = std::process::id();
    let path = root.join(format!(
        "{}-{}.json",
        paths::sanitize_path_segment(cook_id),
        pid
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
        };
        record.capability = "replacement-capability".to_string();
        write_record(&path, &record).expect("replace lease record");
        drop(lease);
        assert!(path.exists(), "a former owner cannot remove a replacement");
    }
}
