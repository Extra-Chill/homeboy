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
//! Everything runs via `sh -c` (POSIX). Windows is out of scope for MVP —
//! the Unix-only internals (`setsid`, `SIGTERM`/`SIGKILL`, `pre_exec`) live
//! inside a `#[cfg(unix)]` module; on Windows the public API compiles but
//! returns `RigServiceFailed` so the crate still builds everywhere.

use super::spec::RigSpec;
use crate::error::Result;

/// Live status of a service as seen at probe time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceStatus {
    Running(u32),
    Stopped,
    /// PID recorded but process is gone — state is stale.
    Stale(u32),
}

/// One discovered process — its PID and start time (seconds since epoch).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredProcess {
    pub pid: u32,
    pub started_at_epoch: u64,
}

/// Start a service if it isn't already running. Idempotent.
///
/// Returns the PID of the running (or newly started) process.
pub fn start(rig: &RigSpec, service_id: &str) -> Result<u32> {
    platform::start(rig, service_id)
}

/// Stop a running service. Idempotent — if not running, returns immediately.
pub fn stop(rig: &RigSpec, service_id: &str) -> Result<()> {
    platform::stop(rig, service_id)
}

/// Report current service status, cross-referencing rig state with live PID.
pub fn status(rig_id: &str, service_id: &str) -> Result<ServiceStatus> {
    platform::status(rig_id, service_id)
}

/// Log file path for a supervised service.
pub fn log_path(rig_id: &str, service_id: &str) -> Result<std::path::PathBuf> {
    platform::log_file_path(rig_id, service_id)
}

/// Find the newest process whose command line contains `pattern`.
///
/// Used by `external` services (`stop` discovery) and the `newer_than`
/// check (`process_start` time source). Returns `Ok(None)` when no
/// process matches — both callers treat that as "nothing to do."
///
/// Implementation: shells out to `ps -axo pid=,lstart=,args=` and filters
/// in Rust. We pick the newest match (largest start-time) so a stale +
/// fresh pair surfaces the fresh one to consumers (the rig wants to know
/// "is the live daemon stale?").
pub fn discover_newest(pattern: &str) -> Result<Option<DiscoveredProcess>> {
    platform::discover_newest(pattern)
}

/// `discover_newest`, but returning the PID only. Convenience used by
/// `service::stop` for the `external` kind.
pub fn discover_external_pid(pattern: &str) -> Result<Option<u32>> {
    Ok(discover_newest(pattern)?.map(|p| p.pid))
}

/// Parse `ps -o etime` output into total seconds.
///
/// Re-exported as a module-scope helper so tests in
/// `tests/core/rig/service_test.rs` can exercise the format parser
/// without needing to drive a real `ps` invocation. Production callers
/// only see this through `discover_newest`.
#[cfg(unix)]
pub fn parse_etime_seconds(s: &str) -> Option<u64> {
    platform::parse_etime_seconds(s)
}

#[cfg(unix)]
mod platform {
    use std::fs::{File, OpenOptions};
    use std::os::unix::process::CommandExt;
    use std::path::PathBuf;
    use std::process::{Command, Stdio};
    use std::thread;
    use std::time::{Duration, Instant};

    use super::super::expand::expand_vars;
    use super::super::spec::{RigSpec, ServiceKind, ServiceSpec};
    use super::super::state::{now_rfc3339, ServiceState};
    use super::ServiceStatus;
    use crate::error::{Error, Result};
    use crate::paths;

    pub fn start(rig: &RigSpec, service_id: &str) -> Result<u32> {
        let spec = rig.services.get(service_id).ok_or_else(|| {
            Error::rig_service_failed(&rig.id, service_id, "service not declared in rig spec")
        })?;

        // External services are adopted, not spawned. Refusing here keeps the
        // contract honest: the rig observes them, it doesn't manage their
        // lifecycle. (`stop` is fine — sending SIGTERM to a discovered PID
        // is a different posture than launching a process you don't own.)
        if spec.kind == ServiceKind::External {
            return Err(Error::rig_service_failed(
                &rig.id,
                service_id,
                "external services are adopted, not spawned — `start` is not supported. Use `stop` to recycle a discovered process.",
            ));
        }

        // Idempotency: if we have a PID and it's live, no-op.
        let mut state = super::super::state::RigState::load(&rig.id)?;
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

    pub fn stop(rig: &RigSpec, service_id: &str) -> Result<()> {
        let spec = rig.services.get(service_id).ok_or_else(|| {
            Error::rig_service_failed(&rig.id, service_id, "service not declared in rig spec")
        })?;
        let mut state = super::super::state::RigState::load(&rig.id)?;

        // For external services, the PID isn't in rig state — we discover it
        // via the configured pattern. No discovery hit ⇒ nothing to stop;
        // that's the desired posture (idempotent: "make sure it's gone").
        let pid = if spec.kind == ServiceKind::External {
            let pattern = spec
                .discover
                .as_ref()
                .map(|d| d.pattern.clone())
                .ok_or_else(|| {
                    Error::rig_service_failed(
                        &rig.id,
                        service_id,
                        "external service requires `discover.pattern`",
                    )
                })?;
            let expanded = expand_vars(rig, &pattern);
            match super::discover_external_pid(&expanded)? {
                Some(pid) => pid,
                None => return Ok(()),
            }
        } else {
            match state.services.get(service_id).and_then(|s| s.pid) {
                Some(pid) => pid,
                None => return Ok(()),
            }
        };

        if !pid_alive(pid) {
            state.services.remove(service_id);
            state.save(&rig.id)?;
            return Ok(());
        }

        let managed_process_group = spec.kind != ServiceKind::External;
        signal(pid, managed_process_group, libc::SIGTERM);

        // Grace period up to 5s.
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if !target_alive(pid, managed_process_group) {
                break;
            }
            thread::sleep(Duration::from_millis(100));
        }

        if target_alive(pid, managed_process_group) {
            signal(pid, managed_process_group, libc::SIGKILL);
            thread::sleep(Duration::from_millis(200));
        }

        state.services.remove(service_id);
        state.save(&rig.id)?;
        Ok(())
    }

    pub fn status(rig_id: &str, service_id: &str) -> Result<ServiceStatus> {
        let state = super::super::state::RigState::load(rig_id)?;
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
                    Error::rig_service_failed(
                        &rig.id,
                        service_id,
                        "command kind requires `command`",
                    )
                })?;
                let expanded = expand_vars(rig, cmd);
                Ok(("sh".to_string(), vec!["-c".to_string(), expanded]))
            }
            ServiceKind::External => {
                // Defensive: `start` short-circuits External upstream so this
                // arm is unreachable in practice. Compiler exhaustiveness still
                // wants it; a clear message beats `unreachable!()` if the
                // upstream guard is ever refactored away.
                Err(Error::rig_service_failed(
                    &rig.id,
                    service_id,
                    "external services cannot be spawned — adoption-only",
                ))
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

    pub(super) fn log_file_path(rig_id: &str, service_id: &str) -> Result<PathBuf> {
        Ok(paths::rig_logs_dir(rig_id)?.join(format!("{}.log", service_id)))
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
        log_file_path(rig_id, service_id)
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

    fn process_group_alive(pgid: u32) -> bool {
        if pgid == 0 {
            return false;
        }
        unsafe { libc::kill(-(pgid as libc::pid_t), 0) == 0 }
    }

    fn target_alive(pid: u32, process_group: bool) -> bool {
        if process_group {
            process_group_alive(pid)
        } else {
            pid_alive(pid)
        }
    }

    fn signal(pid: u32, process_group: bool, sig: libc::c_int) {
        let target = if process_group {
            -(pid as libc::pid_t)
        } else {
            pid as libc::pid_t
        };
        unsafe {
            libc::kill(target, sig);
        }
    }

    /// Find processes whose argv contains `pattern`, return the newest by
    /// start time. Uses `ps -axo pid=,etime=,args=` — `etime` (elapsed
    /// time, format `[[DD-]HH:]MM:SS`) is implemented by both BSD `ps`
    /// (macOS) and procps `ps` (Linux). `etimes` (integer seconds) is
    /// procps-only, so the text format is the portable choice. Newest
    /// match = smallest elapsed seconds.
    pub fn discover_newest(pattern: &str) -> Result<Option<super::DiscoveredProcess>> {
        let output = Command::new("ps")
            .args(["-axo", "pid=,etime=,args="])
            .output()
            .map_err(|e| {
                Error::internal_unexpected(format!("ps for process discovery failed: {}", e))
            })?;
        if !output.status.success() {
            return Err(Error::internal_unexpected(format!(
                "ps exited {}",
                output.status.code().unwrap_or(-1)
            )));
        }

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let self_pid = std::process::id();
        let stdout = String::from_utf8_lossy(&output.stdout);

        let mut newest: Option<super::DiscoveredProcess> = None;
        for line in stdout.lines() {
            // Format: "  <pid>  <etime>  <args...>"
            let trimmed = line.trim_start();
            let pid_end = match trimmed.find(char::is_whitespace) {
                Some(e) => e,
                None => continue,
            };
            let pid: u32 = match trimmed[..pid_end].parse() {
                Ok(p) => p,
                Err(_) => continue,
            };
            let after_pid = trimmed[pid_end..].trim_start();
            let etime_end = match after_pid.find(char::is_whitespace) {
                Some(e) => e,
                None => continue,
            };
            let etime_str = &after_pid[..etime_end];
            let args = after_pid[etime_end..].trim_start();

            if !args.contains(pattern) {
                continue;
            }
            if pid == self_pid {
                continue;
            }

            let elapsed = match parse_etime_seconds(etime_str) {
                Some(s) => s,
                None => continue,
            };
            let started_at = now.saturating_sub(elapsed);
            let candidate = super::DiscoveredProcess {
                pid,
                started_at_epoch: started_at,
            };
            match &newest {
                None => newest = Some(candidate),
                Some(curr) if candidate.started_at_epoch > curr.started_at_epoch => {
                    newest = Some(candidate)
                }
                _ => {}
            }
        }
        Ok(newest)
    }

    /// Parse `ps -o etime` output into total seconds.
    ///
    /// Accepted shapes (BSD `ps` and procps `ps` both produce these):
    /// - `MM:SS`
    /// - `HH:MM:SS`
    /// - `DD-HH:MM:SS`
    /// Anything else returns `None`.
    pub(super) fn parse_etime_seconds(s: &str) -> Option<u64> {
        let (days, rest) = match s.split_once('-') {
            Some((d, r)) => (d.parse::<u64>().ok()?, r),
            None => (0u64, s),
        };
        let parts: Vec<&str> = rest.split(':').collect();
        let (h, m, sec) = match parts.as_slice() {
            [m, s] => (0u64, m.parse::<u64>().ok()?, s.parse::<u64>().ok()?),
            [h, m, s] => (
                h.parse::<u64>().ok()?,
                m.parse::<u64>().ok()?,
                s.parse::<u64>().ok()?,
            ),
            _ => return None,
        };
        Some(days * 86_400 + h * 3_600 + m * 60 + sec)
    }
}

#[cfg(not(unix))]
mod platform {
    //! Non-Unix stub. Rig services rely on POSIX process groups, SIGTERM/
    //! SIGKILL, and `pre_exec` for detached supervision — none of which map
    //! cleanly to Windows job objects. Every entry point returns
    //! `RigServiceFailed` with the same message so callers get a clear
    //! reason instead of a compile error.
    use super::super::spec::RigSpec;
    use super::ServiceStatus;
    use crate::error::{Error, Result};

    const UNSUPPORTED: &str = "rig services are not supported on this platform (Unix only)";

    pub fn start(rig: &RigSpec, service_id: &str) -> Result<u32> {
        Err(Error::rig_service_failed(&rig.id, service_id, UNSUPPORTED))
    }

    pub fn stop(rig: &RigSpec, service_id: &str) -> Result<()> {
        Err(Error::rig_service_failed(&rig.id, service_id, UNSUPPORTED))
    }

    pub fn status(rig_id: &str, service_id: &str) -> Result<ServiceStatus> {
        Err(Error::rig_service_failed(rig_id, service_id, UNSUPPORTED))
    }

    pub fn log_file_path(rig_id: &str, service_id: &str) -> Result<std::path::PathBuf> {
        Err(Error::rig_service_failed(rig_id, service_id, UNSUPPORTED))
    }

    pub fn discover_newest(_pattern: &str) -> Result<Option<super::DiscoveredProcess>> {
        Err(Error::internal_unexpected(UNSUPPORTED))
    }
}

#[cfg(test)]
#[cfg(unix)]
#[path = "../../../tests/core/rig/service_test.rs"]
mod service_test;
