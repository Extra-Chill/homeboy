use std::path::PathBuf;

use crate::core::error::Result;

use super::{homeboy, homeboy_data, sanitize_path_segment};

/// Daemon runtime state directory (~/.config/homeboy/daemon/).
fn daemon_state_dir() -> Result<PathBuf> {
    Ok(homeboy()?.join("daemon"))
}

/// Daemon runtime state file (~/.config/homeboy/daemon/state.json).
pub fn daemon_state_file() -> Result<PathBuf> {
    Ok(daemon_state_dir()?.join("state.json"))
}

/// Daemon durable job state file (~/.config/homeboy/daemon/jobs.json).
pub fn daemon_jobs_file() -> Result<PathBuf> {
    Ok(daemon_state_dir()?.join("jobs.json"))
}

/// Runner connection session state directory (~/.config/homeboy/runner-sessions/).
fn runner_sessions_dir() -> Result<PathBuf> {
    Ok(homeboy()?.join("runner-sessions"))
}

/// Runner connection session state file (~/.config/homeboy/runner-sessions/{id}.json).
pub fn runner_session_file(id: &str) -> Result<PathBuf> {
    Ok(runner_sessions_dir()?.join(format!("{}.json", id)))
}

/// Managed service tunnel runtime state directory (~/.local/share/homeboy/service-tunnels/{id}/).
pub fn service_tunnel_runtime_dir(id: &str) -> Result<PathBuf> {
    Ok(homeboy_data()?
        .join("service-tunnels")
        .join(sanitize_path_segment(id)))
}

/// Managed service tunnel runtime state file.
pub fn service_tunnel_runtime_state_file(id: &str) -> Result<PathBuf> {
    Ok(service_tunnel_runtime_dir(id)?.join("state.json"))
}

/// Preview ingress route declarations (~/.config/homeboy/preview-ingress/routes/).
pub fn preview_ingress_routes_dir() -> Result<PathBuf> {
    Ok(homeboy()?.join("preview-ingress").join("routes"))
}

/// Preview ingress route declaration file.
pub fn preview_ingress_route_file(id: &str) -> Result<PathBuf> {
    Ok(preview_ingress_routes_dir()?.join(format!("{}.json", sanitize_path_segment(id))))
}
