//! Local daemon lifecycle and artifact-fetch orchestration owned by core.
//!
//! The command layer (`src/commands/daemon.rs`) stays a thin adapter: it parses
//! arguments and renders output. The process spawning, status polling, HTTP
//! artifact fetch, and filesystem persistence live here so the orchestration is
//! testable and reusable outside the CLI.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::core::error::{Error, Result};
use crate::core::execution_contract::encode_uri_component;
use crate::core::process::pid_is_running;

use super::{
    acquire_daemon_operation_lock, acquire_daemon_operation_lock_for_ensure, parse_bind_addr,
    read_status, repair_legacy_lease_for_start, stop_unlocked, DaemonStartResult,
    DAEMON_STARTUP_TOKEN_ENV,
};

/// Outcome of a daemon byte-endpoint artifact download.
#[derive(Debug, Clone)]
pub struct ArtifactFetchOutcome {
    pub daemon_url: String,
    pub content_url: String,
    pub output_path: PathBuf,
    pub content_type: Option<String>,
    pub size_bytes: u64,
    pub sha256: Option<String>,
}

/// Spawn the daemon in the background, then poll the state file until the new
/// process publishes its address (or a timeout elapses).
pub fn start_background(addr: &str) -> Result<DaemonStartResult> {
    parse_bind_addr(addr)?;
    let _lock = acquire_daemon_operation_lock()?;
    start_background_unlocked(addr)
}

/// Return a live daemon under the lifecycle lock, or start one when its lease
/// is absent or its recorded PID is dead.
pub fn ensure_running(addr: &str) -> Result<DaemonStartResult> {
    ensure_running_with_wait(addr, Duration::from_secs(5))
}

fn ensure_running_with_wait(addr: &str, wait: Duration) -> Result<DaemonStartResult> {
    parse_bind_addr(addr)?;
    ensure_running_with_operations(
        wait,
        acquire_daemon_operation_lock_for_ensure,
        read_status,
        pid_is_running,
        || start_background_unlocked(addr),
    )
}

fn ensure_running_with_operations<Lock, AcquireLock, ReadStatus, PidIsRunning, Start>(
    wait: Duration,
    acquire_lock: AcquireLock,
    read_status: ReadStatus,
    pid_is_running: PidIsRunning,
    start: Start,
) -> Result<DaemonStartResult>
where
    AcquireLock: FnOnce(Duration) -> Result<Lock>,
    ReadStatus: FnOnce() -> Result<super::DaemonStatus>,
    PidIsRunning: FnOnce(u32) -> bool,
    Start: FnOnce() -> Result<DaemonStartResult>,
{
    let _lock = acquire_lock(wait)?;
    let status = read_status()?;
    if let Some(state) = status.state {
        if pid_is_running(state.pid) {
            return Ok(DaemonStartResult {
                pid: state.pid,
                address: state.address,
                state_path: state.state_path,
                lease_id: state.lease_id,
            });
        }
    }
    start()
}

fn start_background_unlocked(addr: &str) -> Result<DaemonStartResult> {
    let _repaired_legacy_lease = repair_legacy_lease_for_start()?;
    let existing = read_status()?;
    if existing.state.is_some() || existing.stale_reason.is_some() {
        let _ = stop_unlocked()?;
    }

    let exe = std::env::current_exe().map_err(|e| {
        Error::internal_io(
            e.to_string(),
            Some("resolve current executable".to_string()),
        )
    })?;
    let startup_token = uuid::Uuid::new_v4().to_string();
    let child = Command::new(exe)
        .args(["daemon", "serve", "--addr", addr])
        .env(DAEMON_STARTUP_TOKEN_ENV, &startup_token)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| Error::internal_io(e.to_string(), Some("spawn daemon".to_string())))?;
    let pid = child.id();

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let status = read_status()?;
        if let Some(state) = status.state {
            if state.pid == pid && state.startup_token == startup_token {
                return Ok(DaemonStartResult {
                    pid,
                    address: state.address,
                    state_path: state.state_path,
                    lease_id: state.lease_id,
                });
            }
        }

        if Instant::now() >= deadline {
            return Err(Error::internal_unexpected(format!(
                "daemon process {} did not publish matching startup token before timeout",
                pid
            )));
        }

        thread::sleep(Duration::from_millis(50));
    }
}

/// Resolve the daemon base URL, falling back to the running daemon's address.
fn resolve_daemon_url(daemon_url: Option<String>) -> Result<String> {
    if let Some(url) = daemon_url.filter(|url| !url.trim().is_empty()) {
        return Ok(url);
    }
    let status = read_status()?;
    let Some(state) = status.state.filter(|_| status.running) else {
        return Err(Error::validation_invalid_argument(
            "daemon-url",
            "daemon is not running; pass --daemon-url or start it with `homeboy daemon start`",
            None,
            None,
        ));
    };
    Ok(format!("http://{}", state.address))
}

/// Build the encoded daemon byte-endpoint URL for a given run/artifact pair.
pub fn artifact_content_url(daemon_url: &str, run_id: &str, artifact_id: &str) -> Result<String> {
    let mut base = reqwest::Url::parse(daemon_url).map_err(|e| {
        Error::validation_invalid_argument(
            "daemon-url",
            e.to_string(),
            Some(daemon_url.to_string()),
            None,
        )
    })?;
    base.set_path(&format!(
        "/runs/{}/artifacts/{}/content",
        encode_uri_component(run_id),
        encode_uri_component(artifact_id)
    ));
    base.set_query(None);
    Ok(base.to_string())
}

/// Fetch artifact bytes through the local daemon byte endpoint and persist them.
///
/// Resolves the daemon URL, downloads the content, ensures the parent directory
/// exists, and writes the bytes to `output`. Returns metadata describing the
/// download for the caller to render.
pub fn fetch_artifact_to_path(
    run_id: &str,
    artifact_id: &str,
    daemon_url: Option<String>,
    output: Option<PathBuf>,
) -> Result<ArtifactFetchOutcome> {
    let daemon_url = resolve_daemon_url(daemon_url)?;
    let content_url = artifact_content_url(&daemon_url, run_id, artifact_id)?;
    let output_path = output.unwrap_or_else(|| default_artifact_output_path(artifact_id));

    let response = reqwest::blocking::get(&content_url).map_err(reqwest_error)?;
    let status = response.status();
    let headers = response.headers().clone();
    if !status.is_success() {
        let body = response.text().unwrap_or_default();
        return Err(Error::validation_invalid_argument(
            "artifact_id",
            format!(
                "daemon artifact fetch failed with HTTP {}: {}",
                status.as_u16(),
                body
            ),
            Some(artifact_id.to_string()),
            None,
        ));
    }

    let bytes = response.bytes().map_err(reqwest_error)?;
    if let Some(parent) = output_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent).map_err(|e| {
            Error::internal_io(e.to_string(), Some(format!("create {}", parent.display())))
        })?;
    }
    std::fs::write(&output_path, &bytes).map_err(|e| {
        Error::internal_io(
            e.to_string(),
            Some(format!("write {}", output_path.display())),
        )
    })?;

    Ok(ArtifactFetchOutcome {
        daemon_url,
        content_url,
        output_path,
        content_type: header_value(&headers, reqwest::header::CONTENT_TYPE.as_str()),
        size_bytes: bytes.len() as u64,
        sha256: header_value(&headers, "x-homeboy-artifact-sha256"),
    })
}

fn default_artifact_output_path(artifact_id: &str) -> PathBuf {
    artifact_id
        .rsplit('/')
        .find(|segment| !segment.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("artifact.bin"))
}

fn header_value(headers: &reqwest::header::HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

fn reqwest_error(error: reqwest::Error) -> Error {
    Error::internal_io(error.to_string(), Some("fetch daemon artifact".to_string()))
}

#[cfg(test)]
#[path = "../../../tests/core/daemon/control_test.rs"]
mod control_test;
