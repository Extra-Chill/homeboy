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

use std::path::Path;

use serde::Serialize;

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

    /// A real probe of a real path must not panic and must not block a machine
    /// that is fine.
    #[test]
    fn probing_a_real_path_does_not_block_a_healthy_machine() {
        let preflight = preflight_capacity(Path::new("/"), "root", CapacityReserve::NONE);

        assert_ne!(preflight.status, CapacityStatus::Exhausted);
    }
}
