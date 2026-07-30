//! Filesystem disk-budget probing for observation evidence reports.
//!
//! Extracted from the `commands::runs::disk` adapter so the evidence report
//! service and other observation consumers can compute the same disk budget
//! without depending on a CLI command module.
//!
//! Byte capacity is not the only way a filesystem runs out. A workspace mount
//! reached ZERO free inodes with ~34 GB still free, and every byte-only probe
//! reported `ok` right up to hard `ENOSPC` — tests then failed while writing
//! their own resource summary, and Git could not create `index.lock`. Cleanup
//! could not recover it either, because opening the SQLite observation store
//! needs a journal file it had no inode for (#10603).
//!
//! `statvfs` already returns inode counts in the same call that returns block
//! counts, so reporting them costs nothing and closes that blind spot.

use std::path::Path;

use serde::Serialize;

#[derive(Clone, Serialize, Default)]
pub struct DiskBudget {
    pub path: String,
    pub available_bytes: Option<u64>,
    pub total_bytes: Option<u64>,
    pub used_percent: Option<f64>,
    /// Free inodes. A filesystem with capacity in bytes and none here still
    /// fails every write (#10603).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub available_inodes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_inodes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inodes_used_percent: Option<f64>,
    pub status: String,
    pub warning: Option<String>,
}

#[cfg(unix)]
pub fn disk_budget(path: &Path, subject: &str, _unavailable_message: &str) -> DiskBudget {
    let c_path = match std::ffi::CString::new(path.to_string_lossy().as_bytes()) {
        Ok(path) => path,
        Err(_) => return unavailable_disk_budget(path, "path contains an interior NUL byte"),
    };
    let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    let rc = unsafe { libc::statvfs(c_path.as_ptr(), stat.as_mut_ptr()) };
    if rc != 0 {
        return unavailable_disk_budget(path, "statvfs failed");
    }
    let stat = unsafe { stat.assume_init() };
    let block_size = u128::from(stat.f_frsize.max(1));
    let total = u64::try_from(u128::from(stat.f_blocks).saturating_mul(block_size)).ok();
    let available = u64::try_from(u128::from(stat.f_bavail).saturating_mul(block_size)).ok();
    let used_percent = match (total, available) {
        (Some(total), Some(available)) if total > 0 => {
            Some(((total.saturating_sub(available)) as f64 / total as f64) * 100.0)
        }
        _ => None,
    };
    // `f_files` is 0 on filesystems that do not report inodes at all (some
    // network and pseudo filesystems), which must read as "not measured"
    // rather than as total exhaustion.
    let reports_inodes = stat.f_files > 0;
    let total_inodes = reports_inodes.then(|| u64::from(stat.f_files));
    let available_inodes = reports_inodes.then(|| u64::from(stat.f_favail));
    let inodes_used_percent = match (total_inodes, available_inodes) {
        (Some(total), Some(available)) if total > 0 => {
            Some(((total.saturating_sub(available)) as f64 / total as f64) * 100.0)
        }
        _ => None,
    };

    let byte_warning = match (available, total) {
        (Some(available), Some(total)) if total > 0 && available < total / 10 => {
            Some(format!("{subject} filesystem has less than 10% free space"))
        }
        (Some(available), _) if available < 5 * 1024 * 1024 * 1024 => Some(format!(
            "{subject} filesystem has less than 5 GiB free space"
        )),
        _ => None,
    };
    // Inode exhaustion is reported even when bytes look healthy, which is the
    // exact state that produced a silent `ok` before hard ENOSPC.
    let inode_warning = inode_warning_for(subject, available_inodes, total_inodes);
    let warning = combine_warnings(byte_warning, inode_warning);

    DiskBudget {
        path: path.display().to_string(),
        available_bytes: available,
        total_bytes: total,
        used_percent,
        available_inodes,
        total_inodes,
        inodes_used_percent,
        status: if warning.is_some() { "warning" } else { "ok" }.to_string(),
        warning,
    }
}

/// Inode pressure for a probed filesystem.
///
/// Exhaustion is called out separately from "low", and both are reported even
/// when byte capacity is healthy — that combination is precisely what read as
/// `ok` until every write failed (#10603).
#[cfg(unix)]
fn inode_warning_for(
    subject: &str,
    available_inodes: Option<u64>,
    total_inodes: Option<u64>,
) -> Option<String> {
    match (available_inodes, total_inodes) {
        (Some(0), _) => Some(format!(
            "{subject} filesystem has NO free inodes; writes will fail regardless of free bytes"
        )),
        (Some(available), Some(total)) if total > 0 && available < total / 10 => Some(format!(
            "{subject} filesystem has less than 10% free inodes"
        )),
        _ => None,
    }
}

/// Keep both pressures. Reporting only the first would let byte pressure mask
/// inode exhaustion, or the reverse.
#[cfg(unix)]
fn combine_warnings(bytes: Option<String>, inodes: Option<String>) -> Option<String> {
    match (bytes, inodes) {
        (Some(bytes), Some(inodes)) => Some(format!("{bytes}; {inodes}")),
        (Some(only), None) | (None, Some(only)) => Some(only),
        (None, None) => None,
    }
}

#[cfg(not(unix))]
pub fn disk_budget(path: &Path, _subject: &str, unavailable_message: &str) -> DiskBudget {
    unavailable_disk_budget(path, unavailable_message)
}

fn unavailable_disk_budget(path: &Path, warning: &str) -> DiskBudget {
    DiskBudget {
        path: path.display().to_string(),
        available_bytes: None,
        total_bytes: None,
        used_percent: None,
        available_inodes: None,
        total_inodes: None,
        inodes_used_percent: None,
        status: "unknown".to_string(),
        warning: Some(warning.to_string()),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// A real probe must now carry inode counts. `statvfs` already returns them
    /// alongside the block counts, so this costs no extra syscall (#10603).
    #[test]
    fn a_probe_reports_inode_capacity_alongside_bytes() {
        let budget = disk_budget(Path::new("/"), "test", "unavailable");

        assert!(
            budget.total_inodes.is_some() && budget.available_inodes.is_some(),
            "a probed filesystem must report inode capacity: {:?}",
            budget.warning
        );
        assert!(
            budget.inodes_used_percent.is_some(),
            "inode utilisation must be derivable"
        );
    }

    /// The #10603 state: ~34 GB free and ZERO free inodes. A byte-only probe
    /// reported `ok` right up to hard ENOSPC, so cleanup was never prompted and
    /// the store could not open a journal to plan one.
    #[test]
    fn zero_free_inodes_warns_even_when_byte_capacity_is_healthy() {
        let warning = inode_warning_for("workspace", Some(0), Some(13_107_200));

        let warning = warning.expect("no free inodes must warn regardless of free bytes");
        assert!(
            warning.contains("NO free inodes"),
            "the warning must name inode exhaustion explicitly: {warning}"
        );
        assert!(
            warning.contains("regardless of free bytes"),
            "the warning must pre-empt the 'but there is plenty of space' reading: {warning}"
        );
    }

    #[test]
    fn low_free_inodes_warns_before_exhaustion() {
        assert!(inode_warning_for("workspace", Some(1_000), Some(13_107_200)).is_some());
        assert!(inode_warning_for("workspace", Some(9_000_000), Some(13_107_200)).is_none());
    }

    /// Filesystems that do not track inodes report `f_files == 0`. That must
    /// read as "not measured", never as total exhaustion.
    #[test]
    fn a_filesystem_without_inode_accounting_is_not_reported_as_exhausted() {
        assert!(inode_warning_for("workspace", None, None).is_none());
    }

    /// Both pressures must survive into one message; neither may mask the other.
    #[test]
    fn byte_and_inode_warnings_are_both_retained() {
        let combined = combine_warnings(
            Some("low bytes".to_string()),
            Some("low inodes".to_string()),
        );
        let combined = combined.expect("both warnings present");
        assert!(combined.contains("low bytes") && combined.contains("low inodes"));
    }
}
