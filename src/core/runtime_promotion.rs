//! Machine-global serialization for operations that replace or select Homeboy
//! binaries. The directory-create guard follows the established rig lease lock
//! convention, while the JSON record makes a blocked writer actionable.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::core::build_identity;
use crate::core::error::{Error, Result};
use crate::core::paths;

const LEASE_DIR: &str = "promotion.lock";
const LEASE_FILE: &str = "lease.json";
const PIN_DIR: &str = "pins";
const DEFAULT_TTL: Duration = Duration::from_secs(30 * 60);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimePromotionLeaseRecord {
    pub schema: String,
    pub pid: u32,
    pub operation: String,
    pub target: String,
    pub generation: String,
    pub started_at: String,
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
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

impl Drop for RuntimeGenerationPinGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// Acquire the global writer lease. A nested call from the same process is
/// allowed so a refresh can reconnect its daemon without opening a race.
pub fn acquire(operation: &str, target: impl Into<String>) -> Result<RuntimePromotionLease> {
    let target = target.into();
    let root = paths::runtime_promotion_dir()?;
    fs::create_dir_all(&root).map_err(io("create runtime promotion directory"))?;
    let path = root.join(LEASE_DIR);
    let pid = std::process::id();
    let generation = current_generation();

    if let Some(pin) = active_pin(&root)? {
        return Err(Error::validation_invalid_argument(
            "runtime_generation_pin",
            format!(
                "runtime promotion waits for active cook `{}` (pid {}) pinned to generation `{}`",
                pin.cook_id, pin.pid, pin.generation
            ),
            Some(pin.cook_id),
            Some(vec![
                "Follow: `homeboy activity` and retry after the cook finalizes.".to_string(),
            ]),
        ));
    }

    match fs::create_dir(&path) {
        Ok(()) => write_record(
            &path,
            &RuntimePromotionLeaseRecord {
                schema: "homeboy/runtime-promotion-lease/v1".to_string(),
                pid,
                operation: operation.to_string(),
                target,
                generation: generation.clone(),
                started_at: now(),
            },
        )
        .map(|_| RuntimePromotionLease {
            path,
            primary: true,
            generation,
        }),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let held = read_record(&path)?;
            if held.pid == pid {
                return Ok(RuntimePromotionLease {
                    path,
                    primary: false,
                    generation,
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

fn read_record(path: &Path) -> Result<RuntimePromotionLeaseRecord> {
    let content =
        fs::read_to_string(path.join(LEASE_FILE)).map_err(io("read runtime promotion lease"))?;
    serde_json::from_str(&content).map_err(|e| {
        Error::validation_invalid_json(e, Some("parse runtime promotion lease".to_string()), None)
    })
}

fn blocked_error(held: &RuntimePromotionLeaseRecord, reclaimable: bool) -> Error {
    let age = age_seconds(&held.started_at).unwrap_or(-1);
    let action = if reclaimable {
        "The holder is dead or expired; run `homeboy runtime promotion-takeover` to record an explicit takeover."
    } else {
        "Wait for the owner to finish, then follow with `homeboy self status`."
    };
    Error::validation_invalid_argument(
        "runtime_promotion_lease",
        format!(
            "runtime promotion is held by pid {} operation `{}` target `{}` for {}s",
            held.pid, held.operation, held.target, age
        ),
        Some(held.target.clone()),
        Some(vec![
            action.to_string(),
            "Follow: `homeboy self doctor`".to_string(),
        ]),
    )
}

fn reclaimable(record: &RuntimePromotionLeaseRecord) -> bool {
    !crate::core::process::pid_is_running(record.pid)
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
        if !crate::core::process::pid_is_running(pin.pid)
            || age_seconds(&pin.started_at).is_some_and(|age| age >= DEFAULT_TTL.as_secs() as i64)
        {
            let _ = fs::remove_file(path);
        }
    }
    Ok(())
}

fn active_pin(root: &Path) -> Result<Option<RuntimeGenerationPin>> {
    let pins = root.join(PIN_DIR);
    if !pins.exists() {
        return Ok(None);
    }
    prune_pins(&pins)?;
    for entry in fs::read_dir(&pins).map_err(io("read runtime generation pins"))? {
        let path = entry.map_err(io("read runtime generation pin"))?.path();
        let content = fs::read_to_string(path).map_err(io("read runtime generation pin"))?;
        if let Ok(pin) = serde_json::from_str(&content) {
            return Ok(Some(pin));
        }
    }
    Ok(None)
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
        };
        let error = blocked_error(&held, false);
        assert!(error.message.contains("pid 42"));
        assert!(error.message.contains("runner refresh"));
        assert!(error.message.contains("lab"));
        assert!(format!("{:?}", error).contains("self status"));
    }
}
