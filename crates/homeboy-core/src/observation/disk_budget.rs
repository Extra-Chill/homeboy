//! Filesystem disk-budget probing for observation evidence reports.
//!
//! Extracted from the `commands::runs::disk` adapter so the evidence report
//! service and other observation consumers can compute the same disk budget
//! without depending on a CLI command module. Output shape is unchanged.

use std::path::Path;

use serde::Serialize;

#[derive(Clone, Serialize)]
pub struct DiskBudget {
    pub path: String,
    pub available_bytes: Option<u64>,
    pub total_bytes: Option<u64>,
    pub used_percent: Option<f64>,
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
    let warning = match (available, total) {
        (Some(available), Some(total)) if total > 0 && available < total / 10 => {
            Some(format!("{subject} filesystem has less than 10% free space"))
        }
        (Some(available), _) if available < 5 * 1024 * 1024 * 1024 => Some(format!(
            "{subject} filesystem has less than 5 GiB free space"
        )),
        _ => None,
    };
    DiskBudget {
        path: path.display().to_string(),
        available_bytes: available,
        total_bytes: total,
        used_percent,
        status: if warning.is_some() { "warning" } else { "ok" }.to_string(),
        warning,
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
        status: "unknown".to_string(),
        warning: Some(warning.to_string()),
    }
}
