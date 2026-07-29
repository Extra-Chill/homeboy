use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fs4::fs_std::FileExt;
use serde::{Deserialize, Serialize};

use super::{cleanup_shared_cargo_targets, CargoTargetCleanupOptions, CargoTargetCleanupOutput};
use crate::defaults::RetentionConfig;
use crate::{Error, Result};

const STATE_FILE: &str = "automatic-retention.json";
const LOCK_FILE: &str = "automatic-retention.lock";

// POSIX advisory locks are process-scoped, so a second thread in this process
// can otherwise re-enter the file lock. The file lock covers other processes.
static IN_PROCESS_ADMISSION: OnceLock<Mutex<()>> = OnceLock::new();

#[derive(Debug, Serialize)]
pub struct AutomaticRetentionOutput {
    pub command: &'static str,
    pub status: &'static str,
    pub max_run_seconds: u64,
    pub row_limit: usize,
    pub state_path: String,
    pub resume_command: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cargo_targets: Option<CargoTargetCleanupOutput>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct AutomaticRetentionState {
    last_started_unix_ms: u64,
    last_finished_unix_ms: u64,
    cursor: Option<String>,
    status: String,
}

pub fn run_automatic_cargo_retention() -> Result<AutomaticRetentionOutput> {
    let retention = crate::defaults::load_config().retention;
    let data = homeboy_paths::homeboy_data()?;
    run_automatic_cargo_retention_in(&retention, &data, None, SystemTime::now())
}

fn run_automatic_cargo_retention_in(
    retention: &RetentionConfig,
    data: &Path,
    cargo_root: Option<PathBuf>,
    now: SystemTime,
) -> Result<AutomaticRetentionOutput> {
    let state_path = data.join(STATE_FILE);
    let base = output_base(retention, &state_path);
    let admission = IN_PROCESS_ADMISSION
        .get_or_init(|| Mutex::new(()))
        .try_lock();
    let Ok(_admission) = admission else {
        return Ok(AutomaticRetentionOutput {
            status: "busy",
            ..base
        });
    };
    fs::create_dir_all(data)
        .map_err(|error| io_error(error, "create retention state directory"))?;
    let lock = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(data.join(LOCK_FILE))
        .map_err(|error| io_error(error, "open automatic retention lock"))?;
    if lock.try_lock_exclusive().is_err() {
        return Ok(AutomaticRetentionOutput {
            status: "busy",
            ..base
        });
    }

    let mut state = read_state(&state_path);
    state.last_started_unix_ms = unix_ms(now);
    state.status = "running".to_string();
    write_state(&state_path, &state)?;

    let output = cleanup_shared_cargo_targets(CargoTargetCleanupOptions {
        root: cargo_root,
        apply: true,
        older_than: Duration::from_secs(retention.shared_store_days.saturating_mul(86_400)),
        lease_ttl: Duration::from_secs(retention.shared_store_lease_seconds),
        max_bytes: retention.shared_store_max_bytes,
        limit: usize::try_from(retention.limit).unwrap_or(0),
        cursor: state.cursor.clone(),
        now,
        deadline: now.checked_add(Duration::from_secs(
            retention.automatic_retention_max_run_seconds,
        )),
    })?;
    state.cursor = output.next_cursor.clone();
    state.last_finished_unix_ms = unix_ms(SystemTime::now());
    state.status = if output.applied_count == 0 && output.continuation_required {
        "no_progress".to_string()
    } else if output.continuation_required {
        "partial".to_string()
    } else {
        "completed".to_string()
    };
    write_state(&state_path, &state)?;
    let status = match state.status.as_str() {
        "no_progress" => "no_progress",
        "partial" => "partial",
        _ => "completed",
    };
    Ok(AutomaticRetentionOutput {
        status,
        cargo_targets: Some(output),
        ..base
    })
}

fn output_base(retention: &RetentionConfig, state_path: &Path) -> AutomaticRetentionOutput {
    AutomaticRetentionOutput {
        command: "cleanup.automatic_retention",
        status: "busy",
        max_run_seconds: retention.automatic_retention_max_run_seconds,
        row_limit: usize::try_from(retention.limit).unwrap_or(0),
        state_path: state_path.display().to_string(),
        resume_command: "homeboy cleanup automatic-retention".to_string(),
        cargo_targets: None,
    }
}

fn unix_ms(now: SystemTime) -> u64 {
    now.duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn read_state(path: &Path) -> AutomaticRetentionState {
    fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

fn write_state(path: &Path, state: &AutomaticRetentionState) -> Result<()> {
    let raw = serde_json::to_string_pretty(state).map_err(|error| {
        Error::internal_json(
            error.to_string(),
            Some("serialize retention state".to_string()),
        )
    })?;
    fs::write(path, raw).map_err(|error| io_error(error, "write automatic retention state"))
}

fn io_error(error: std::io::Error, operation: &str) -> Error {
    Error::internal_io(error.to_string(), Some(operation.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cleanup::cargo_targets::acquire_shared_cargo_target_in;
    use tempfile::TempDir;

    static TEST_SERIAL: OnceLock<Mutex<()>> = OnceLock::new();

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

    #[test]
    fn cargo_budget_converges_across_bounded_resumable_passes() {
        let _serial = TEST_SERIAL
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let data = TempDir::new().unwrap();
        let cargo = TempDir::new().unwrap();
        let now = SystemTime::now();
        let first = store(cargo.path(), "first", 4, now);
        let second = store(cargo.path(), "second", 4, now);
        let mut config = enabled_retention();
        config.shared_store_max_bytes = 0;

        let first_pass = run_automatic_cargo_retention_in(
            &config,
            data.path(),
            Some(cargo.path().to_path_buf()),
            now,
        )
        .unwrap();
        assert_eq!(first_pass.status, "partial");
        assert_eq!(
            first_pass.cargo_targets.as_ref().unwrap().inspected_count,
            1
        );
        assert_ne!(first.exists(), second.exists());
        let second_pass = run_automatic_cargo_retention_in(
            &config,
            data.path(),
            Some(cargo.path().to_path_buf()),
            now,
        )
        .unwrap();
        assert_eq!(second_pass.status, "completed");
        assert!(!second.exists());
    }

    #[test]
    fn active_lease_is_protected_and_bounded_pass_records_no_progress() {
        let _serial = TEST_SERIAL
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let data = TempDir::new().unwrap();
        let cargo = TempDir::new().unwrap();
        let now = SystemTime::now();
        let lease = acquire_shared_cargo_target_in(cargo.path(), "active", now).unwrap();
        fs::write(lease.target_dir().join("artifact"), b"payload").unwrap();
        let other = store(cargo.path(), "other", 8, now);
        let output = run_automatic_cargo_retention_in(
            &enabled_retention(),
            data.path(),
            Some(cargo.path().to_path_buf()),
            now,
        )
        .unwrap();
        assert_eq!(output.status, "no_progress");
        assert_eq!(output.cargo_targets.as_ref().unwrap().inspected_count, 1);
        assert!(lease.target_dir().exists());
        assert!(other.exists());
        assert!(data.path().join(STATE_FILE).exists());
    }

    #[test]
    fn concurrent_pass_is_admission_rejected_without_mutating_state() {
        let _serial = TEST_SERIAL
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let data = TempDir::new().unwrap();
        let cargo = TempDir::new().unwrap();
        let _admission = IN_PROCESS_ADMISSION
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap();

        let output = run_automatic_cargo_retention_in(
            &enabled_retention(),
            data.path(),
            Some(cargo.path().to_path_buf()),
            SystemTime::now(),
        )
        .unwrap();

        assert_eq!(output.status, "busy");
        assert!(!data.path().join(STATE_FILE).exists());
    }
}
