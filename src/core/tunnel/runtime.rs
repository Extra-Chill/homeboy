use std::fs;
use std::process::{Child, Command};
use std::time::Duration;

use crate::core::error::{Error, Result};
use crate::core::paths;
use crate::core::process::{pid_is_running, process_group_is_running};

use super::types::*;

pub(super) fn shell_command(command: &str) -> Command {
    #[cfg(windows)]
    {
        let mut cmd = Command::new("cmd");
        cmd.args(["/C", command]);
        cmd
    }

    #[cfg(not(windows))]
    {
        let mut cmd = Command::new("/bin/sh");
        cmd.args(["-c", command]);
        cmd
    }
}

pub(super) fn process_group_id_for(pid: u32) -> Option<i32> {
    #[cfg(unix)]
    unsafe {
        let pgid = libc::getpgid(pid as libc::pid_t);
        if pgid > 0 {
            return Some(pgid);
        }
        None
    }

    #[cfg(not(unix))]
    {
        let _ = pid;
        None
    }
}

pub(super) fn load_runtime_state(id: &str) -> Result<Option<ServiceTunnelRuntimeState>> {
    let path = paths::service_tunnel_runtime_state_file(id)?;
    if !path.exists() {
        return Ok(None);
    }
    let data = fs::read_to_string(&path)
        .map_err(|e| Error::internal_io(e.to_string(), Some(path.display().to_string())))?;
    serde_json::from_str(&data)
        .map(Some)
        .map_err(|e| Error::internal_json(e.to_string(), Some(path.display().to_string())))
}

pub(super) fn save_runtime_state(state: &ServiceTunnelRuntimeState) -> Result<()> {
    let path = paths::service_tunnel_runtime_state_file(&state.preview_identity.service_id)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| Error::internal_io(e.to_string(), Some(parent.display().to_string())))?;
    }
    let data = serde_json::to_string_pretty(state).map_err(|e| {
        Error::internal_json(
            e.to_string(),
            Some(state.preview_identity.service_id.clone()),
        )
    })?;
    fs::write(&path, data)
        .map_err(|e| Error::internal_io(e.to_string(), Some(path.display().to_string())))
}

pub(super) fn remove_runtime_state(id: &str) -> Result<()> {
    let path = paths::service_tunnel_runtime_state_file(id)?;
    if path.exists() {
        fs::remove_file(&path)
            .map_err(|e| Error::internal_io(e.to_string(), Some(path.display().to_string())))?;
    }
    Ok(())
}

/// A runtime state paired with the liveness observation that was made when it
/// was refreshed. Both `running` and `backend_running` are captured in a single
/// pass so that downstream status derivation is internally consistent: the
/// process can exit between OS liveness checks, so re-querying liveness while
/// building the status report can otherwise produce a state that reports
/// `running == false` yet still surfaces readiness/process details. Capturing
/// the snapshot once removes that race.
pub(super) struct LiveRuntimeState {
    pub(super) state: ServiceTunnelRuntimeState,
    pub(super) running: bool,
    pub(super) backend_running: bool,
}

pub(super) fn refresh_runtime_state(id: &str) -> Result<Option<LiveRuntimeState>> {
    let Some(state) = load_runtime_state(id)? else {
        return Ok(None);
    };
    let running = runtime_state_is_running(&state);
    let backend_running = backend_state_is_running(&state);
    if running || backend_running {
        return Ok(Some(LiveRuntimeState {
            state,
            running,
            backend_running,
        }));
    }

    terminate_backend_state(&state)?;
    remove_runtime_state(id)?;
    Ok(None)
}

pub(super) fn ensure_supervised_process_still_running(
    state: &ServiceTunnelRuntimeState,
    child: &mut Child,
) -> Result<()> {
    std::thread::sleep(Duration::from_millis(100));
    match child.try_wait() {
        Ok(Some(status)) => Err(Error::validation_invalid_argument(
            "service",
            "service process exited after becoming ready",
            Some(state.preview_identity.service_id.clone()),
            Some(vec![format!("exit status: {status}")]),
        )),
        Ok(None) => Ok(()),
        Err(error) => Err(Error::internal_io(
            error.to_string(),
            Some(format!(
                "check service tunnel process {}",
                state.preview_identity.service_id
            )),
        )),
    }
}

pub(super) fn runtime_state_is_running(state: &ServiceTunnelRuntimeState) -> bool {
    if let Some(pgid) = state.process.process_group_id {
        process_group_is_running(pgid)
    } else {
        pid_is_running(state.pid)
    }
}

pub(super) fn backend_state_is_running(state: &ServiceTunnelRuntimeState) -> bool {
    state
        .backend_process
        .as_ref()
        .is_some_and(backend_process_is_running)
}

pub(super) fn backend_process_is_running(state: &ServiceTunnelBackendProcessState) -> bool {
    if let Some(pgid) = state.process.process_group_id {
        process_group_is_running(pgid)
    } else {
        pid_is_running(state.pid)
    }
}

pub(super) fn terminate_backend_state(state: &ServiceTunnelRuntimeState) -> Result<()> {
    let Some(backend) = &state.backend_process else {
        return Ok(());
    };
    if !backend_process_is_running(backend) {
        return Ok(());
    }

    #[cfg(unix)]
    unsafe {
        if let Some(pgid) = backend.process.process_group_id {
            libc::kill(-(pgid as libc::pid_t), libc::SIGTERM);
            std::thread::sleep(Duration::from_millis(250));
            if process_group_is_running(pgid) {
                libc::kill(-(pgid as libc::pid_t), libc::SIGKILL);
            }
        } else {
            libc::kill(backend.pid as libc::pid_t, libc::SIGTERM);
        }
    }

    #[cfg(not(unix))]
    {
        let _ = backend;
    }

    Ok(())
}

pub(super) fn terminate_runtime_state(state: &ServiceTunnelRuntimeState) -> Result<()> {
    if !runtime_state_is_running(state) {
        return Ok(());
    }

    #[cfg(unix)]
    unsafe {
        if let Some(pgid) = state.process.process_group_id {
            libc::kill(-(pgid as libc::pid_t), libc::SIGTERM);
            std::thread::sleep(Duration::from_millis(250));
            if process_group_is_running(pgid) {
                libc::kill(-(pgid as libc::pid_t), libc::SIGKILL);
            }
        } else {
            libc::kill(state.pid as libc::pid_t, libc::SIGTERM);
        }
    }

    #[cfg(not(unix))]
    {
        let _ = state;
    }

    Ok(())
}

pub(super) fn local_url_for(tunnel: &ServiceTunnel) -> String {
    match tunnel.local_port {
        Some(port) => format!("{}://{}:{}", tunnel.scheme, tunnel.local_host, port),
        None => format!("{}://{}:<auto>", tunnel.scheme, tunnel.local_host),
    }
}

pub(super) fn resolve_health_url(
    tunnel: &ServiceTunnel,
    health_url: Option<String>,
    health_path: Option<String>,
) -> Option<String> {
    if let Some(url) = health_url {
        return Some(url);
    }
    health_path.map(|path| {
        let normalized = if path.starts_with('/') {
            path
        } else {
            format!("/{path}")
        };
        format!("{}{}", local_url_for(tunnel), normalized)
    })
}

pub(super) fn runtime_evidence(state: &ServiceTunnelRuntimeState) -> ServiceTunnelEvidence {
    ServiceTunnelEvidence {
        state_path: paths::service_tunnel_runtime_state_file(&state.preview_identity.service_id)
            .map(|path| path.display().to_string())
            .unwrap_or_default(),
        logs: state.logs.clone(),
    }
}
