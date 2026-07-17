use std::path::PathBuf;

use homeboy_error::Result;

use super::{homeboy, homeboy_data, sanitize_path_segment};

/// Daemon runtime state directory (~/.config/homeboy/daemon/).
fn daemon_state_dir() -> Result<PathBuf> {
    Ok(homeboy()?.join("daemon"))
}

/// Daemon runtime state file (~/.config/homeboy/daemon/state.json).
pub fn daemon_state_file() -> Result<PathBuf> {
    Ok(daemon_state_dir()?.join("state.json"))
}

/// Machine-global coordination directory for Homeboy runtime binary promotion.
pub fn runtime_promotion_dir() -> Result<PathBuf> {
    Ok(homeboy_data()?.join("runtime-promotion"))
}

/// Daemon durable job state file (~/.config/homeboy/daemon/jobs.json).
pub fn daemon_jobs_file() -> Result<PathBuf> {
    Ok(daemon_state_dir()?.join("jobs.json"))
}

/// Latest bounded launcher-owned termination evidence for this daemon store.
pub fn daemon_termination_file() -> Result<PathBuf> {
    Ok(daemon_state_dir()?.join("termination.json"))
}

/// Exact state-loss recovery receipt keyed by the operator-supplied lease.
pub fn daemon_state_loss_recovery_receipt_file(lease_id: &str) -> Result<PathBuf> {
    Ok(daemon_state_dir()?
        .join("state-loss-recovery")
        .join(format!("{}.json", sanitize_path_segment(lease_id))))
}

/// Exact replacement receipt for an approved lease-less recovery.
pub fn daemon_leaseless_recovery_receipt_file() -> Result<PathBuf> {
    Ok(daemon_state_dir()?.join("leaseless-recovery.json"))
}

/// Runner connection session state directory (~/.config/homeboy/runner-sessions/).
pub fn runner_sessions_dir() -> Result<PathBuf> {
    Ok(homeboy()?.join("runner-sessions"))
}

/// Runner connection session state file (~/.config/homeboy/runner-sessions/{id}.json).
pub fn runner_session_file(id: &str) -> Result<PathBuf> {
    Ok(runner_sessions_dir()?.join(format!("{}.json", id)))
}

/// Controller-local runner connection state. The runner-level file remains the
/// lease record for the remote daemon; local tunnels belong to one controller.
pub fn runner_controller_session_file(id: &str, controller_id: &str) -> Result<PathBuf> {
    Ok(runner_sessions_dir()?
        .join(sanitize_path_segment(id))
        .join(format!("{}.json", sanitize_path_segment(controller_id))))
}

/// Controller-owned lease evidence retained after an ephemeral runner session
/// or remote daemon state file disappears.
pub(crate) fn runner_lease_evidence_file(runner_id: &str, lease_id: &str) -> Result<PathBuf> {
    Ok(homeboy()?
        .join("runner-lease-evidence")
        .join(sanitize_path_segment(runner_id))
        .join(format!("{}.json", sanitize_path_segment(lease_id))))
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
