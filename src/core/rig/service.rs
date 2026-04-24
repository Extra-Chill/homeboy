//! Service supervisor for rigs — start/stop/health-check local processes.
//!
//! MVP keeps this deliberately small. Two kinds of services:
//!
//! - `http-static` — `python3 -m http.server <port>` in a cwd. The common
//!   case for dev envs that need to serve tarballs / static assets locally.
//! - `command` — arbitrary shell command.
//!
//! Lifecycle:
//! - `start` forks the process detached (so it survives `homeboy` exit),
//!   records the PID in rig state, and appends stdout/stderr to a log file.
//! - `stop` sends SIGTERM, waits up to 5s, then SIGKILL.
//! - `status` checks whether the recorded PID is alive.
//!
//! Everything runs via `sh -c` (POSIX). Windows is out of scope for MVP.

use std::fs::{File, OpenOptions};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use super::expand::expand_vars;
use super::spec::{RigSpec, ServiceKind, ServiceSpec};
use super::state::{now_rfc3339, ServiceState};
use crate::error::{Error, Result};
use crate::paths;

/// Live status of a service as seen at probe time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceStatus {
    Running(u32),
    Stopped,
    /// PID recorded but process is gone — state is stale.
    Stale(u32),
}

/// Start a service if it isn't already running. Idempotent.
///
/// Returns the PID of the running (or newly started) process.
pub fn start(rig: &RigSpec, service_id: &str) -> Result<u32> {
    let spec = rig.services.get(service_id).ok_or_else(|| {
        Error::rig_service_failed(&rig.id, service_id, "service not declared in rig spec")
    })?;

    // Idempotency: if we have a PID and it's live, no-op.
    let mut state = super::state::RigState::load(&rig.id)?;
    if let Some(svc_state) = state.services.get(service_id) {
        if let Some(pid) = svc_state.pid {
            if pid_alive(pid) {
                return Ok(pid);
            }
        }
    }

    let (program, args) = build_command(rig, service_id, spec)?;
    let cwd = resolve_cwd(rig, spec)?;
    let log_path = log_file_for(&rig.id, service_id)?;
    let log_file = open_log(&log_path)?;
    let err_file = log_file
        .try_clone()
        .map_err(|e| Error::internal_unexpected(format!("failed to clone log fd: {}", e)))?;

    let mut command = Command::new(&program);
    command
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log_file))
        .stderr(Stdio::from(err_file));

    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }

    for (k, v) in &spec.env {
        command.env(k, expand_vars(rig, v));
    }

    // Detach from homeboy — new session so Ctrl-C to `homeboy` doesn't kill it.
    // Safe: setsid has no allocations and only touches the child.
    unsafe {
        command.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }

    let child = command.spawn().map_err(|e| {
        Error::rig_service_failed(&rig.id, service_id, format!("spawn failed: {}", e))
    })?;

    let pid = child.id();

    // We intentionally leak the Child handle — once detached, we track by PID
    // in rig state, not by owning the handle. Dropping it without wait()
    // leaves a zombie briefly until the next `rig down`, which is acceptable
    // for a dev supervisor.
    std::mem::forget(child);

    state.services.insert(
        service_id.to_string(),
        ServiceState {
            pid: Some(pid),
            started_at: Some(now_rfc3339()),
            status: "running".to_string(),
        },
    );
    state.save(&rig.id)?;

    Ok(pid)
}

/// Stop a running service. Idempotent — if not running, returns immediately.
pub fn stop(rig: &RigSpec, service_id: &str) -> Result<()> {
    let mut state = super::state::RigState::load(&rig.id)?;
    let pid = match state.services.get(service_id).and_then(|s| s.pid) {
        Some(pid) => pid,
        None => return Ok(()),
    };

    if !pid_alive(pid) {
        state.services.remove(service_id);
        state.save(&rig.id)?;
        return Ok(());
    }

    unsafe {
        libc::kill(pid as libc::pid_t, libc::SIGTERM);
    }

    // Grace period up to 5s.
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if !pid_alive(pid) {
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }

    if pid_alive(pid) {
        unsafe {
            libc::kill(pid as libc::pid_t, libc::SIGKILL);
        }
        thread::sleep(Duration::from_millis(200));
    }

    state.services.remove(service_id);
    state.save(&rig.id)?;
    Ok(())
}

/// Report current service status, cross-referencing rig state with live PID.
pub fn status(rig_id: &str, service_id: &str) -> Result<ServiceStatus> {
    let state = super::state::RigState::load(rig_id)?;
    let pid = match state.services.get(service_id).and_then(|s| s.pid) {
        Some(pid) => pid,
        None => return Ok(ServiceStatus::Stopped),
    };

    if pid_alive(pid) {
        Ok(ServiceStatus::Running(pid))
    } else {
        Ok(ServiceStatus::Stale(pid))
    }
}

/// Build (program, args) for a service kind.
fn build_command(
    rig: &RigSpec,
    service_id: &str,
    spec: &ServiceSpec,
) -> Result<(String, Vec<String>)> {
    match spec.kind {
        ServiceKind::HttpStatic => {
            let port = spec.port.ok_or_else(|| {
                Error::rig_service_failed(&rig.id, service_id, "http-static requires `port`")
            })?;
            Ok((
                "python3".to_string(),
                vec![
                    "-m".to_string(),
                    "http.server".to_string(),
                    port.to_string(),
                ],
            ))
        }
        ServiceKind::Command => {
            let cmd = spec.command.as_ref().ok_or_else(|| {
                Error::rig_service_failed(&rig.id, service_id, "command kind requires `command`")
            })?;
            let expanded = expand_vars(rig, cmd);
            Ok(("sh".to_string(), vec!["-c".to_string(), expanded]))
        }
    }
}

fn resolve_cwd(rig: &RigSpec, spec: &ServiceSpec) -> Result<Option<PathBuf>> {
    match &spec.cwd {
        None => Ok(None),
        Some(cwd) => {
            let expanded = expand_vars(rig, cwd);
            let path = shellexpand::tilde(&expanded).into_owned();
            Ok(Some(PathBuf::from(path)))
        }
    }
}

fn log_file_for(rig_id: &str, service_id: &str) -> Result<PathBuf> {
    let dir = paths::rig_logs_dir(rig_id)?;
    std::fs::create_dir_all(&dir).map_err(|e| {
        Error::internal_unexpected(format!(
            "Failed to create logs dir {}: {}",
            dir.display(),
            e
        ))
    })?;
    Ok(dir.join(format!("{}.log", service_id)))
}

fn open_log(path: &PathBuf) -> Result<File> {
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| {
            Error::internal_unexpected(format!("Failed to open log {}: {}", path.display(), e))
        })
}

/// Cheap liveness probe — `kill(pid, 0)` returns 0 if the process exists and
/// we have permission to signal it. Matches what `ps` and most supervisors do.
fn pid_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}
