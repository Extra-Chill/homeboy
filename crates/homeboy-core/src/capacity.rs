//! Capacity preflight: turn the disk budget into a decision.
//!
//! [`crate::observation::disk_budget`] already probes free bytes *and* free
//! inodes in one `statvfs`, and it is correct. It was also, until this module,
//! wired to nothing: both of its call sites are report-only. Nothing consulted
//! it before materializing bytes. A measurement that no decision reads cannot
//! change an outcome, which is why the #10603 fix did not prevent a recurrence
//! (#11127).
//!
//! # The ladder is deliberately lopsided
//!
//! A preflight that fails closed can block legitimate work, and blocking work
//! on a filesystem that is merely *tight* is worse than the failure it
//! prevents. So:
//!
//! * **Exhausted** — hard error — only when a *measured* value is exactly zero.
//!   Zero free bytes or zero free inodes means the next write fails; refusing
//!   before writing is strictly better than a half-written artifact that then
//!   has to be reaped.
//! * **Warning** — never blocks — when a measured value is below the configured
//!   reserve. The reserve keys already exist in
//!   [`crate::defaults::RetentionConfig`] and are already default-on
//!   (`shared_store_reserve_bytes` 5 GiB, `shared_store_reserve_inodes` 100k),
//!   so this invents no new threshold.
//! * **Unknown** — never blocks. A filesystem that does not report inodes, or a
//!   `statvfs` that failed, must read as "not measured", never as exhaustion.
//!
//! Only a definite zero blocks. Everything uncertain proceeds.

use std::collections::hash_map::DefaultHasher;
use std::fs::{self, File, OpenOptions};
use std::hash::{Hash, Hasher};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::defaults::RetentionConfig;
use crate::error::StorageExhaustedDetails;
use crate::observation::disk_budget::{disk_budget, DiskBudget};
use crate::{Error, Result};

/// Free capacity a caller wants left over after its write.
///
/// Both keys already exist in [`RetentionConfig`] and are consulted today by
/// exactly one consumer, the shared build-store admission check. Reusing them
/// keeps one configured reserve rather than growing a second vocabulary for the
/// same question.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct CapacityReserve {
    pub bytes: u64,
    pub inodes: u64,
}

/// The bytes and filesystem entries a materialization is expected to create.
///
/// Entries are deliberately counted independently from bytes: dependency trees
/// such as `node_modules` and `vendor` commonly exhaust inodes first.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct CapacityDemand {
    pub bytes: u64,
    pub inodes: u64,
}

impl CapacityDemand {
    pub fn saturating_add(self, other: Self) -> Self {
        Self {
            bytes: self.bytes.saturating_add(other.bytes),
            inodes: self.inodes.saturating_add(other.inodes),
        }
    }
}

/// Measure a tree that will be copied or reconstructed before materialization.
/// Symlinks are one entry and are not followed. Generated build/cache roots are
/// excluded because they are reclaimable artifacts, not source demand; this
/// keeps admission bounded to the materialized source/dependency tree.
pub fn demand_for_tree(path: &Path) -> Result<CapacityDemand> {
    let metadata = std::fs::symlink_metadata(path).map_err(|error| {
        Error::internal_io(error.to_string(), Some(format!("stat {}", path.display())))
    })?;
    let mut demand = CapacityDemand {
        bytes: metadata.len(),
        inodes: 1,
    };
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        for entry in std::fs::read_dir(path).map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(format!("read directory {}", path.display())),
            )
        })? {
            let entry = entry.map_err(|error| {
                Error::internal_io(
                    error.to_string(),
                    Some(format!("read directory {}", path.display())),
                )
            })?;
            if matches!(
                entry.file_name().to_str(),
                Some(".git" | "target" | "cache" | ".cache")
            ) {
                continue;
            }
            demand = demand.saturating_add(demand_for_tree(&entry.path())?);
        }
    }
    Ok(demand)
}

const RESERVATION_TTL: Duration = Duration::from_secs(30 * 60);

/// A durable cross-process capacity claim. Dropping it releases the exact
/// record on every terminal path, including cancellation and pre-execution
/// failure.
#[derive(Debug)]
pub struct CapacityReservation {
    ledger: PathBuf,
    id: String,
}

impl Drop for CapacityReservation {
    fn drop(&mut self) {
        let _ = remove_capacity_reservation(&self.ledger, &self.id);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CapacityReservationRecord {
    id: String,
    filesystem: String,
    root: String,
    bytes: u64,
    inodes: u64,
    owner_pid: u32,
    owner_process: String,
    created_unix_seconds: u64,
    lease_expires_unix_seconds: u64,
}

struct CapacityReservationInput<'a> {
    ledger: &'a Path,
    root: &'a Path,
    filesystem: &'a str,
    subject: &'a str,
    budget: DiskBudget,
    demand: CapacityDemand,
    reserve: CapacityReserve,
    owner: (u32, String),
    now: u64,
}

/// Reserve projected materialization demand before the first large write.
///
/// Capacity is compared after subtracting already-reserved capacity and this
/// request, then against the configured floor. The reservation is intentionally
/// process-local: it closes concurrent Cook overcommit while the durable
/// lifecycle remains the authority for recovering interrupted work.
pub fn reserve_projected_capacity(
    path: &Path,
    subject: &str,
    demand: CapacityDemand,
    reserve: CapacityReserve,
) -> Result<CapacityReservation> {
    let root = existing_ancestor(path);
    let filesystem = filesystem_identity(&root)?;
    let ledger = capacity_ledger_path(&filesystem)?;
    let budget = disk_budget(
        &root,
        subject,
        "capacity is not measurable on this platform",
    );
    reserve_projected_capacity_in(CapacityReservationInput {
        ledger: &ledger,
        root: &root,
        filesystem: &filesystem,
        subject,
        budget,
        demand,
        reserve,
        owner: owner_evidence(std::process::id()),
        now: now_seconds(),
    })
}

fn reserve_projected_capacity_in(
    input: CapacityReservationInput<'_>,
) -> Result<CapacityReservation> {
    let lock = lock_capacity_ledger(input.ledger)?;
    let mut records = read_capacity_reservations(input.ledger)?;
    records.retain(|record| reservation_is_live(record, input.now));
    let held = records
        .iter()
        .fold(CapacityDemand::default(), |total, record| {
            total.saturating_add(CapacityDemand {
                bytes: record.bytes,
                inodes: record.inodes,
            })
        });
    let projected_bytes = input.budget.available_bytes.map(|available| {
        available
            .saturating_sub(held.bytes)
            .saturating_sub(input.demand.bytes)
    });
    let projected_inodes = input.budget.available_inodes.map(|available| {
        available
            .saturating_sub(held.inodes)
            .saturating_sub(input.demand.inodes)
    });
    let bytes_ok = projected_bytes.is_some_and(|available| available >= input.reserve.bytes);
    let inodes_ok = projected_inodes.is_some_and(|available| available >= input.reserve.inodes);
    if !bytes_ok || !inodes_ok {
        let mut error = Error::storage_exhausted_detailed(StorageExhaustedDetails {
            error: format!(
                "{} projected materialization would breach configured capacity floors",
                input.subject
            ),
            context: Some(format!("admission before {}", input.subject)),
            path: Some(input.root.display().to_string()),
            available_bytes: input.budget.available_bytes,
            available_inodes: input.budget.available_inodes,
            reserve_bytes: Some(input.reserve.bytes),
            reserve_inodes: Some(input.reserve.inodes),
        });
        error.details["projected_bytes"] = serde_json::json!(projected_bytes);
        error.details["projected_inodes"] = serde_json::json!(projected_inodes);
        error.details["demand_bytes"] = serde_json::json!(input.demand.bytes);
        error.details["demand_inodes"] = serde_json::json!(input.demand.inodes);
        error.details["reserved_bytes"] = serde_json::json!(held.bytes);
        error.details["reserved_inodes"] = serde_json::json!(held.inodes);
        error.details["dominant_reclaimable_categories"] = serde_json::json!(["build_output"]);
        let command = format!(
            "homeboy cleanup artifacts --path {} --sort size --limit 100 --apply",
            crate::engine::shell::quote_arg(&input.root.display().to_string())
        );
        error.details["cleanup_commands"] = serde_json::json!([command.clone()]);
        error = error.with_hint(format!(
            "Reclaim the dominant `build_output` category with `{command}`."
        ));
        return Err(error);
    }
    let id = uuid::Uuid::new_v4().to_string();
    records.push(CapacityReservationRecord {
        id: id.clone(),
        filesystem: input.filesystem.to_string(),
        root: input.root.display().to_string(),
        bytes: input.demand.bytes,
        inodes: input.demand.inodes,
        owner_pid: input.owner.0,
        owner_process: input.owner.1,
        created_unix_seconds: input.now,
        lease_expires_unix_seconds: input.now.saturating_add(RESERVATION_TTL.as_secs()),
    });
    write_capacity_reservations(input.ledger, &records)?;
    drop(lock);
    Ok(CapacityReservation {
        ledger: input.ledger.to_path_buf(),
        id,
    })
}

fn existing_ancestor(path: &Path) -> PathBuf {
    let mut current = path;
    while !current.exists() {
        let Some(parent) = current.parent() else {
            break;
        };
        current = parent;
    }
    current
        .canonicalize()
        .unwrap_or_else(|_| current.to_path_buf())
}

fn capacity_ledger_path(filesystem: &str) -> Result<PathBuf> {
    let root = crate::paths::homeboy_data()?.join("controller-state/capacity-reservations");
    fs::create_dir_all(&root).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("create capacity reservation ledger".to_string()),
        )
    })?;
    let mut hasher = DefaultHasher::new();
    filesystem.hash(&mut hasher);
    Ok(root.join(format!("{:016x}.json", hasher.finish())))
}

fn lock_capacity_ledger(ledger: &Path) -> Result<File> {
    let lock_path = ledger.with_extension("lock");
    let lock = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        // Each client reopens this lock before reading and publishing the ledger.
        // Retaining its contents preserves that shared synchronization point.
        .truncate(false)
        .open(&lock_path)
        .map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("open capacity reservation lock".to_string()),
            )
        })?;
    crate::config::lock_exclusive_bounded(&lock, &lock_path, "lock capacity reservation ledger")?;
    Ok(lock)
}

fn read_capacity_reservations(ledger: &Path) -> Result<Vec<CapacityReservationRecord>> {
    match fs::read_to_string(ledger) {
        Ok(value) => serde_json::from_str(&value).map_err(|error| {
            let mut error = Error::internal_json(
                error.to_string(),
                Some("parse capacity reservation ledger".to_string()),
            );
            error.message = "Failed to parse capacity reservation ledger".to_string();
            error
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(Error::internal_io(
            error.to_string(),
            Some("read capacity reservation ledger".to_string()),
        )),
    }
}

fn write_capacity_reservations(ledger: &Path, records: &[CapacityReservationRecord]) -> Result<()> {
    let bytes = serde_json::to_vec(records).map_err(|error| {
        Error::internal_json(
            error.to_string(),
            Some("encode capacity reservation ledger".to_string()),
        )
    })?;
    let mut staged =
        tempfile::NamedTempFile::new_in(ledger.parent().unwrap_or_else(|| Path::new(".")))
            .map_err(|error| {
                Error::internal_io(
                    error.to_string(),
                    Some("stage capacity reservation ledger".to_string()),
                )
            })?;
    staged
        .write_all(&bytes)
        .and_then(|_| staged.as_file().sync_all())
        .map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("write capacity reservation ledger".to_string()),
            )
        })?;
    staged.persist(ledger).map_err(|error| {
        Error::internal_io(
            error.error.to_string(),
            Some("publish capacity reservation ledger".to_string()),
        )
    })?;
    Ok(())
}

fn remove_capacity_reservation(ledger: &Path, id: &str) -> Result<()> {
    let _lock = lock_capacity_ledger(ledger)?;
    let mut records = read_capacity_reservations(ledger)?;
    records.retain(|record| record.id != id);
    write_capacity_reservations(ledger, &records)
}

fn reservation_is_live(record: &CapacityReservationRecord, now: u64) -> bool {
    record.lease_expires_unix_seconds > now && crate::process::pid_is_running(record.owner_pid)
}

fn owner_evidence(pid: u32) -> (u32, String) {
    (pid, format!("pid:{pid}"))
}

fn now_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(unix)]
fn filesystem_identity(path: &Path) -> Result<String> {
    use std::ffi::CString;

    let path = CString::new(path.as_os_str().as_encoded_bytes())
        .map_err(|error| Error::internal_unexpected(error.to_string()))?;
    let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    if unsafe { libc::statvfs(path.as_ptr(), stat.as_mut_ptr()) } != 0 {
        return Err(Error::internal_io(
            std::io::Error::last_os_error().to_string(),
            Some("identify capacity filesystem".to_string()),
        ));
    }
    Ok(format!("fsid:{}", unsafe { stat.assume_init() }.f_fsid))
}

#[cfg(not(unix))]
fn filesystem_identity(path: &Path) -> Result<String> {
    Ok(format!("path:{}", path.display()))
}

impl CapacityReserve {
    /// No reserve. Only a measured zero is then treated as a problem.
    pub const NONE: Self = Self {
        bytes: 0,
        inodes: 0,
    };

    pub fn from_retention(retention: &RetentionConfig) -> Self {
        Self {
            bytes: retention.shared_store_reserve_bytes,
            inodes: retention.shared_store_reserve_inodes,
        }
    }

    /// The configured reserve, resolved from the loaded configuration.
    pub fn configured() -> Self {
        Self::from_retention(&crate::defaults::load_config().retention)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CapacityStatus {
    /// Measured, and above every configured reserve.
    Ok,
    /// Measured, and below a configured reserve. Advisory: never blocks.
    Warning,
    /// A measured value is exactly zero. The next write fails.
    Exhausted,
    /// Not measurable here. Never blocks — see the module docs.
    Unknown,
}

#[derive(Debug, Clone, Serialize)]
pub struct CapacityPreflight {
    /// What the caller was about to do, used verbatim in the message.
    pub subject: String,
    pub path: String,
    pub status: CapacityStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub available_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub available_inodes: Option<u64>,
    pub reserve_bytes: u64,
    pub reserve_inodes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}

impl CapacityPreflight {
    pub fn is_exhausted(&self) -> bool {
        self.status == CapacityStatus::Exhausted
    }

    /// The blocking error, when the filesystem has definitively run out.
    ///
    /// Carries the measured capacity and the reserve that was in force, so the
    /// operator does not have to re-probe to understand the refusal.
    pub fn error(&self) -> Option<Error> {
        if !self.is_exhausted() {
            return None;
        }
        Some(Error::storage_exhausted_detailed(StorageExhaustedDetails {
            error: self
                .warning
                .clone()
                .unwrap_or_else(|| format!("{} filesystem has no capacity left", self.subject)),
            context: Some(format!("preflight before {}", self.subject)),
            path: Some(self.path.clone()),
            available_bytes: self.available_bytes,
            available_inodes: self.available_inodes,
            reserve_bytes: Some(self.reserve_bytes),
            reserve_inodes: Some(self.reserve_inodes),
        }))
    }

    /// `Err` when exhausted, otherwise the advisory warning, if any.
    ///
    /// The two arms are deliberately different types: a warning is data the
    /// caller may surface, an exhaustion is a refusal it must propagate.
    pub fn into_result(self) -> Result<Option<String>> {
        match self.error() {
            Some(error) => Err(error),
            None => Ok(self.warning),
        }
    }
}

/// Probe `path` and decide whether a write may proceed.
///
/// One `statvfs`, through the same [`disk_budget`] the evidence reports use, so
/// a preflight can never disagree with what the report shows.
pub fn preflight_capacity(
    path: &Path,
    subject: &str,
    reserve: CapacityReserve,
) -> CapacityPreflight {
    let budget = disk_budget(path, subject, "capacity is not measurable on this platform");
    preflight_from_budget(budget, subject, reserve)
}

/// The decision, separated from the probe so every branch is reachable in a
/// test without staging a full filesystem.
fn preflight_from_budget(
    budget: DiskBudget,
    subject: &str,
    reserve: CapacityReserve,
) -> CapacityPreflight {
    // A measured zero in either dimension. Inodes are checked independently of
    // bytes because the #10603 state had ~34 GB free and zero free inodes: a
    // byte-only test reads `ok` right up to hard ENOSPC.
    let exhausted_bytes = budget.available_bytes == Some(0);
    let exhausted_inodes = budget.available_inodes == Some(0);
    let below_byte_reserve = reserve.bytes > 0
        && budget
            .available_bytes
            .is_some_and(|available| available < reserve.bytes);
    let below_inode_reserve = reserve.inodes > 0
        && budget
            .available_inodes
            .is_some_and(|available| available < reserve.inodes);

    let status = if exhausted_bytes || exhausted_inodes {
        CapacityStatus::Exhausted
    } else if below_byte_reserve || below_inode_reserve {
        CapacityStatus::Warning
    } else if budget.available_bytes.is_none() && budget.available_inodes.is_none() {
        CapacityStatus::Unknown
    } else {
        CapacityStatus::Ok
    };

    let warning = match status {
        CapacityStatus::Exhausted => Some(exhaustion_message(
            subject,
            exhausted_bytes,
            exhausted_inodes,
        )),
        CapacityStatus::Warning => Some(reserve_message(
            subject,
            below_byte_reserve,
            below_inode_reserve,
            reserve,
        )),
        // Preserve whatever the probe itself had to say (an unavailable probe
        // explains why), rather than manufacturing a second opinion.
        CapacityStatus::Ok | CapacityStatus::Unknown => budget.warning.clone(),
    };

    CapacityPreflight {
        subject: subject.to_string(),
        path: budget.path,
        status,
        available_bytes: budget.available_bytes,
        available_inodes: budget.available_inodes,
        reserve_bytes: reserve.bytes,
        reserve_inodes: reserve.inodes,
        warning,
    }
}

/// Name the dimension that ran out. "Disk full" with 34 GB free reads as a bug
/// report against the tool rather than a fact about the filesystem.
fn exhaustion_message(subject: &str, bytes: bool, inodes: bool) -> String {
    match (bytes, inodes) {
        (true, true) => format!("{subject} filesystem has no free bytes and no free inodes"),
        (false, true) => format!(
            "{subject} filesystem has NO free inodes; writes will fail regardless of free bytes"
        ),
        _ => format!("{subject} filesystem has no free bytes"),
    }
}

fn reserve_message(subject: &str, bytes: bool, inodes: bool, reserve: CapacityReserve) -> String {
    match (bytes, inodes) {
        (true, true) => format!(
            "{subject} filesystem is below both configured reserves ({} bytes, {} inodes)",
            reserve.bytes, reserve.inodes
        ),
        (false, true) => format!(
            "{subject} filesystem is below the configured inode reserve ({})",
            reserve.inodes
        ),
        _ => format!(
            "{subject} filesystem is below the configured free-space reserve ({} bytes)",
            reserve.bytes
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reserve_projected_capacity_from_budget(
        path: &Path,
        subject: &str,
        budget: DiskBudget,
        demand: CapacityDemand,
        reserve: CapacityReserve,
    ) -> Result<CapacityReservation> {
        let ledger = path.join("capacity-reservations.json");
        reserve_projected_capacity_in(CapacityReservationInput {
            ledger: &ledger,
            root: path,
            filesystem: "fixture-filesystem",
            subject,
            budget,
            demand,
            reserve,
            owner: owner_evidence(std::process::id()),
            now: now_seconds(),
        })
    }

    fn budget(available_bytes: Option<u64>, available_inodes: Option<u64>) -> DiskBudget {
        DiskBudget {
            path: "/fixture".to_string(),
            available_bytes,
            available_inodes,
            ..DiskBudget::default()
        }
    }

    fn reserve() -> CapacityReserve {
        CapacityReserve {
            bytes: 5 * 1024 * 1024 * 1024,
            inodes: 100_000,
        }
    }

    /// The #10603 state exactly: plenty of bytes, zero free inodes. A byte-only
    /// gate reads `ok` here, which is how the outage reached hard ENOSPC with
    /// nothing having warned.
    #[test]
    fn zero_free_inodes_blocks_even_with_tens_of_gigabytes_free() {
        let preflight = preflight_from_budget(
            budget(Some(34 * 1024 * 1024 * 1024), Some(0)),
            "artifact publication",
            reserve(),
        );

        assert_eq!(preflight.status, CapacityStatus::Exhausted);
        let error = preflight.error().expect("exhaustion must block");
        assert!(error.is_storage_exhausted());
        assert_eq!(error.details["available_inodes"], 0);
        assert_eq!(error.details["reserve_inodes"], 100_000);
        assert!(
            error.details["error"]
                .as_str()
                .expect("message")
                .contains("regardless of free bytes"),
            "the message must pre-empt the 'but there is plenty of space' reading"
        );
    }

    #[test]
    fn zero_free_bytes_blocks() {
        let preflight = preflight_from_budget(
            budget(Some(0), Some(500_000)),
            "build",
            CapacityReserve::NONE,
        );

        assert_eq!(preflight.status, CapacityStatus::Exhausted);
        assert!(preflight.into_result().is_err());
    }

    /// A preflight that fails closed can block legitimate work. Breaching the
    /// configured reserve is a warning, never a refusal — the reserve exists to
    /// prompt cleanup, not to stop the machine at 5 GiB free.
    #[test]
    fn breaching_the_configured_reserve_warns_but_never_blocks() {
        let preflight = preflight_from_budget(
            budget(Some(1024 * 1024 * 1024), Some(50_000)),
            "artifact publication",
            reserve(),
        );

        assert_eq!(preflight.status, CapacityStatus::Warning);
        assert!(preflight.error().is_none());
        let warning = preflight.into_result().expect("a warning must not block");
        let warning = warning.expect("a breached reserve must say so");
        assert!(
            warning.contains("below both configured reserves"),
            "{warning}"
        );
    }

    /// The reserve keys already exist and are already default-on. The preflight
    /// must read them, not invent its own numbers.
    #[test]
    fn the_reserve_comes_from_the_keys_that_already_exist_in_retention_config() {
        let configured = CapacityReserve::from_retention(&RetentionConfig::default());

        assert_eq!(configured.bytes, 5 * 1024 * 1024 * 1024);
        assert_eq!(configured.inodes, 100_000);
    }

    /// Filesystems that do not track inodes report none. That must read as "not
    /// measured", never as total exhaustion — the inverse would block every
    /// write on a network mount.
    #[test]
    fn an_unmeasurable_filesystem_never_blocks() {
        let preflight = preflight_from_budget(budget(None, None), "build", reserve());

        assert_eq!(preflight.status, CapacityStatus::Unknown);
        assert!(preflight.error().is_none());
    }

    /// Bytes measured and healthy, inodes not reported at all: the byte
    /// measurement still counts, and the missing one must not be read as zero.
    #[test]
    fn a_missing_inode_measurement_is_not_read_as_zero() {
        let preflight = preflight_from_budget(
            budget(Some(500 * 1024 * 1024 * 1024), None),
            "build",
            reserve(),
        );

        assert_eq!(preflight.status, CapacityStatus::Ok);
        assert!(preflight.warning.is_none());
    }

    #[test]
    fn a_healthy_filesystem_is_ok_and_silent() {
        let preflight = preflight_from_budget(
            budget(Some(500 * 1024 * 1024 * 1024), Some(9_000_000)),
            "artifact publication",
            reserve(),
        );

        assert_eq!(preflight.status, CapacityStatus::Ok);
        assert_eq!(preflight.into_result().expect("no block"), None);
    }

    #[test]
    fn large_dependency_tree_near_the_floor_is_rejected_before_materialization() {
        let dir = tempfile::tempdir().expect("capacity fixture");
        let demand = CapacityDemand {
            bytes: 4 * 1024 * 1024 * 1024,
            inodes: 250_000,
        };
        let error = reserve_projected_capacity_from_budget(
            dir.path(),
            "Cook workspace materialization",
            budget(Some(8 * 1024 * 1024 * 1024), Some(500_000)),
            demand,
            reserve(),
        )
        .expect_err("node_modules/vendor-sized demand must not cross the floor");

        assert_eq!(error.details["demand_bytes"], demand.bytes);
        assert_eq!(error.details["demand_inodes"], demand.inodes);
        assert_eq!(
            error.details["dominant_reclaimable_categories"][0],
            "build_output"
        );
        assert_eq!(
            error.details["cleanup_commands"][0],
            format!(
                "homeboy cleanup artifacts --path {} --sort size --limit 100 --apply",
                crate::engine::shell::quote_arg(&dir.path().display().to_string())
            )
        );
    }

    #[test]
    fn sufficient_projected_capacity_acquires_a_reservation() {
        let dir = tempfile::tempdir().expect("capacity fixture");
        let reservation = reserve_projected_capacity_from_budget(
            dir.path(),
            "Cook workspace materialization",
            budget(Some(20 * 1024 * 1024 * 1024), Some(800_000)),
            CapacityDemand {
                bytes: 4 * 1024 * 1024 * 1024,
                inodes: 250_000,
            },
            reserve(),
        )
        .expect("capacity above the post-demand floor admits the Cook");
        drop(reservation);
    }

    #[test]
    fn projected_inode_exhaustion_blocks_even_when_bytes_are_sufficient() {
        let dir = tempfile::tempdir().expect("capacity fixture");
        let error = reserve_projected_capacity_from_budget(
            dir.path(),
            "Cook workspace materialization",
            budget(Some(100 * 1024 * 1024 * 1024), Some(150_000)),
            CapacityDemand {
                bytes: 1,
                inodes: 60_000,
            },
            reserve(),
        )
        .expect_err("inode floor must be enforced independently");

        assert_eq!(error.details["projected_inodes"], 90_000);
        assert_eq!(error.details["reserve_inodes"], 100_000);
    }

    #[test]
    fn dropping_a_reservation_releases_capacity_for_the_next_cook() {
        let dir = tempfile::tempdir().expect("capacity fixture");
        let demand = CapacityDemand {
            bytes: 4 * 1024 * 1024 * 1024,
            inodes: 1,
        };
        let reservation = reserve_projected_capacity_from_budget(
            dir.path(),
            "first Cook",
            budget(Some(10 * 1024 * 1024 * 1024), Some(500_000)),
            demand,
            reserve(),
        )
        .expect("first Cook reserves capacity");
        let blocked = reserve_projected_capacity_from_budget(
            dir.path(),
            "second Cook",
            budget(Some(10 * 1024 * 1024 * 1024), Some(500_000)),
            demand,
            reserve(),
        );
        assert!(blocked.is_err(), "live reservation prevents overcommit");

        drop(reservation);
        reserve_projected_capacity_from_budget(
            dir.path(),
            "second Cook",
            budget(Some(10 * 1024 * 1024 * 1024), Some(500_000)),
            demand,
            reserve(),
        )
        .expect("terminal release admits the next Cook");
    }

    #[test]
    fn independently_opened_ledgers_cannot_overcommit_a_live_filesystem_claim() {
        let dir = tempfile::tempdir().expect("capacity fixture");
        let demand = CapacityDemand {
            bytes: 4 * 1024 * 1024 * 1024,
            inodes: 1,
        };
        let first = reserve_projected_capacity_from_budget(
            dir.path(),
            "first independent Cook",
            budget(Some(10 * 1024 * 1024 * 1024), Some(500_000)),
            demand,
            reserve(),
        )
        .expect("first ledger client reserves capacity");
        let second = reserve_projected_capacity_from_budget(
            dir.path(),
            "second independent Cook",
            budget(Some(10 * 1024 * 1024 * 1024), Some(500_000)),
            demand,
            reserve(),
        );
        assert!(
            second.is_err(),
            "second ledger client observes the first claim"
        );
        drop(first);
    }

    #[test]
    fn dead_or_expired_reservations_are_reconciled_before_admission() {
        let dir = tempfile::tempdir().expect("capacity fixture");
        let ledger = dir.path().join("capacity-reservations.json");
        write_capacity_reservations(
            &ledger,
            &[CapacityReservationRecord {
                id: "crashed-owner".to_string(),
                filesystem: "fixture-filesystem".to_string(),
                root: dir.path().display().to_string(),
                bytes: 100 * 1024 * 1024 * 1024,
                inodes: 900_000,
                owner_pid: u32::MAX,
                owner_process: "pid:4294967295".to_string(),
                created_unix_seconds: now_seconds().saturating_sub(1),
                lease_expires_unix_seconds: now_seconds().saturating_add(RESERVATION_TTL.as_secs()),
            }],
        )
        .expect("seed dead reservation");

        let _replacement = reserve_projected_capacity_from_budget(
            dir.path(),
            "replacement Cook",
            budget(Some(10 * 1024 * 1024 * 1024), Some(500_000)),
            CapacityDemand {
                bytes: 1,
                inodes: 1,
            },
            reserve(),
        )
        .expect("authoritatively dead owner is reclaimable");
        let records = read_capacity_reservations(&ledger).expect("read reconciled ledger");
        assert_eq!(records.len(), 1, "only the live replacement remains");
        assert_ne!(records[0].id, "crashed-owner");
    }

    #[test]
    fn malformed_ledger_fails_closed_without_materialization() {
        let dir = tempfile::tempdir().expect("capacity fixture");
        fs::write(dir.path().join("capacity-reservations.json"), "not json")
            .expect("seed malformed ledger");

        let error = reserve_projected_capacity_from_budget(
            dir.path(),
            "Cook workspace materialization",
            budget(Some(100 * 1024 * 1024 * 1024), Some(900_000)),
            CapacityDemand {
                bytes: 1,
                inodes: 1,
            },
            reserve(),
        )
        .expect_err("corrupt reservation state must block writes");
        assert!(error.message.contains("parse capacity reservation ledger"));
    }

    /// A real probe of a real path must not panic and must not block a machine
    /// that is fine.
    #[test]
    fn probing_a_real_path_does_not_block_a_healthy_machine() {
        let preflight = preflight_capacity(Path::new("/"), "root", CapacityReserve::NONE);

        assert_ne!(preflight.status, CapacityStatus::Exhausted);
    }
}
