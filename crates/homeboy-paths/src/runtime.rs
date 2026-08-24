use std::path::{Path, PathBuf};

use homeboy_error::Result;

use super::{homeboy, homeboy_data, sanitize_path_segment};

/// Daemon runtime state directory (~/.config/homeboy/daemon/).
/// Override only the daemon's lease/job store. This deliberately leaves HOME
/// and normal config resolution intact for generation-scoped daemon processes.
pub const DAEMON_STATE_DIR_ENV: &str = "HOMEBOY_DAEMON_STATE_DIR";

/// The `DAEMON_STATE_DIR_ENV` override, if it names a non-blank directory.
fn daemon_state_dir_override() -> Option<PathBuf> {
    std::env::var(DAEMON_STATE_DIR_ENV)
        .ok()
        .filter(|path| !path.trim().is_empty())
        .map(PathBuf::from)
}

/// Daemon runtime state directory below an already-resolved config root.
///
/// `DAEMON_STATE_DIR_ENV` still outranks the supplied root, exactly as it
/// outranks `homeboy()` on the ambient path — the injected root replaces only
/// the historical default.
fn daemon_state_dir_in_root(config_root: &Path) -> PathBuf {
    daemon_state_dir_override().unwrap_or_else(|| config_root.join("daemon"))
}

/// The config root the ambient daemon helpers hang from.
///
/// When `DAEMON_STATE_DIR_ENV` is set, `daemon_state_dir_in_root` replaces the
/// whole daemon directory and never joins its `config_root` argument into a
/// path — so home resolution is skipped rather than run and discarded. That
/// keeps a generation-scoped daemon carrying an explicit state directory from
/// newly failing on an unresolvable home, which is the pre-injection behavior.
fn ambient_daemon_config_root() -> Result<PathBuf> {
    if daemon_state_dir_override().is_some() {
        return Ok(PathBuf::new());
    }
    homeboy()
}

/// Daemon runtime state file below an already-resolved config root.
pub fn daemon_state_file_in_root(config_root: &Path) -> PathBuf {
    daemon_state_dir_in_root(config_root).join("state.json")
}

/// Daemon runtime state file (~/.config/homeboy/daemon/state.json).
pub fn daemon_state_file() -> Result<PathBuf> {
    Ok(daemon_state_file_in_root(&ambient_daemon_config_root()?))
}

/// Machine-global coordination directory for Homeboy runtime binary promotion,
/// below an already-resolved data root.
pub fn runtime_promotion_dir_in_root(data_root: &Path) -> PathBuf {
    data_root.join("runtime-promotion")
}

/// Machine-global coordination directory for Homeboy runtime binary promotion.
pub fn runtime_promotion_dir() -> Result<PathBuf> {
    Ok(runtime_promotion_dir_in_root(&homeboy_data()?))
}

/// Daemon durable job state file below an already-resolved config root.
pub fn daemon_jobs_file_in_root(config_root: &Path) -> PathBuf {
    daemon_state_dir_in_root(config_root).join("jobs.json")
}

/// Daemon durable job state file (~/.config/homeboy/daemon/jobs.json).
pub fn daemon_jobs_file() -> Result<PathBuf> {
    Ok(daemon_jobs_file_in_root(&ambient_daemon_config_root()?))
}

/// Latest bounded launcher-owned termination evidence below an already-resolved
/// config root.
pub fn daemon_termination_file_in_root(config_root: &Path) -> PathBuf {
    daemon_state_dir_in_root(config_root).join("termination.json")
}

/// Latest bounded launcher-owned termination evidence for this daemon store.
pub fn daemon_termination_file() -> Result<PathBuf> {
    Ok(daemon_termination_file_in_root(
        &ambient_daemon_config_root()?,
    ))
}

/// Exact state-loss recovery receipt below an already-resolved config root.
pub fn daemon_state_loss_recovery_receipt_file_in_root(
    config_root: &Path,
    lease_id: &str,
) -> PathBuf {
    daemon_state_dir_in_root(config_root)
        .join("state-loss-recovery")
        .join(format!("{}.json", sanitize_path_segment(lease_id)))
}

/// Exact state-loss recovery receipt keyed by the operator-supplied lease.
pub fn daemon_state_loss_recovery_receipt_file(lease_id: &str) -> Result<PathBuf> {
    Ok(daemon_state_loss_recovery_receipt_file_in_root(
        &ambient_daemon_config_root()?,
        lease_id,
    ))
}

/// Exact replacement receipt for an approved lease-less recovery, below an
/// already-resolved config root.
pub fn daemon_leaseless_recovery_receipt_file_in_root(config_root: &Path) -> PathBuf {
    daemon_state_dir_in_root(config_root).join("leaseless-recovery.json")
}

/// Exact replacement receipt for an approved lease-less recovery.
pub fn daemon_leaseless_recovery_receipt_file() -> Result<PathBuf> {
    Ok(daemon_leaseless_recovery_receipt_file_in_root(
        &ambient_daemon_config_root()?,
    ))
}

/// Runner connection session state directory below an already-resolved config root.
pub fn runner_sessions_dir_in_root(config_root: &Path) -> PathBuf {
    config_root.join("runner-sessions")
}

/// Runner connection session state directory (~/.config/homeboy/runner-sessions/).
pub fn runner_sessions_dir() -> Result<PathBuf> {
    Ok(runner_sessions_dir_in_root(&homeboy()?))
}

/// Runner connection session state file below an already-resolved config root.
pub fn runner_session_file_in_root(config_root: &Path, id: &str) -> PathBuf {
    runner_sessions_dir_in_root(config_root).join(format!("{}.json", id))
}

/// Runner connection session state file (~/.config/homeboy/runner-sessions/{id}.json).
pub fn runner_session_file(id: &str) -> Result<PathBuf> {
    Ok(runner_session_file_in_root(&homeboy()?, id))
}

/// Controller-local runner connection state below an already-resolved config root.
pub fn runner_controller_session_file_in_root(
    config_root: &Path,
    id: &str,
    controller_id: &str,
) -> PathBuf {
    runner_sessions_dir_in_root(config_root)
        .join(sanitize_path_segment(id))
        .join(format!("{}.json", sanitize_path_segment(controller_id)))
}

/// Controller-local runner connection state. The runner-level file remains the
/// lease record for the remote daemon; local tunnels belong to one controller.
pub fn runner_controller_session_file(id: &str, controller_id: &str) -> Result<PathBuf> {
    Ok(runner_controller_session_file_in_root(
        &homeboy()?,
        id,
        controller_id,
    ))
}

/// Runner-owned durable reverse-execution evidence below an already-resolved
/// config root.
pub fn runner_job_execution_context_evidence_file_in_root(
    config_root: &Path,
    runner_id: &str,
    context_id: &str,
) -> PathBuf {
    config_root
        .join("runner-job-execution-context")
        .join(sanitize_path_segment(runner_id))
        .join(format!("{}.json", sanitize_path_segment(context_id)))
}

/// Runner-owned durable reverse-execution evidence. This is distinct from the
/// controller's job event record so a restarted runner can resolve the exact
/// broker-issued context it persisted before starting a child process.
pub fn runner_job_execution_context_evidence_file(
    runner_id: &str,
    context_id: &str,
) -> Result<PathBuf> {
    Ok(runner_job_execution_context_evidence_file_in_root(
        &homeboy()?,
        runner_id,
        context_id,
    ))
}

/// Managed service tunnel runtime state directory below an already-resolved
/// data root.
pub fn service_tunnel_runtime_dir_in_root(data_root: &Path, id: &str) -> PathBuf {
    data_root
        .join("service-tunnels")
        .join(sanitize_path_segment(id))
}

/// Managed service tunnel runtime state directory (~/.local/share/homeboy/service-tunnels/{id}/).
pub fn service_tunnel_runtime_dir(id: &str) -> Result<PathBuf> {
    Ok(service_tunnel_runtime_dir_in_root(&homeboy_data()?, id))
}

/// Managed service tunnel runtime state file below an already-resolved data root.
pub fn service_tunnel_runtime_state_file_in_root(data_root: &Path, id: &str) -> PathBuf {
    service_tunnel_runtime_dir_in_root(data_root, id).join("state.json")
}

/// Managed service tunnel runtime state file.
pub fn service_tunnel_runtime_state_file(id: &str) -> Result<PathBuf> {
    Ok(service_tunnel_runtime_state_file_in_root(
        &homeboy_data()?,
        id,
    ))
}

/// Preview ingress route declarations below an already-resolved config root.
pub fn preview_ingress_routes_dir_in_root(config_root: &Path) -> PathBuf {
    config_root.join("preview-ingress").join("routes")
}

/// Preview ingress route declarations (~/.config/homeboy/preview-ingress/routes/).
pub fn preview_ingress_routes_dir() -> Result<PathBuf> {
    Ok(preview_ingress_routes_dir_in_root(&homeboy()?))
}

/// Preview ingress route declaration file below an already-resolved config root.
pub fn preview_ingress_route_file_in_root(config_root: &Path, id: &str) -> PathBuf {
    preview_ingress_routes_dir_in_root(config_root)
        .join(format!("{}.json", sanitize_path_segment(id)))
}

/// Preview ingress route declaration file.
pub fn preview_ingress_route_file(id: &str) -> Result<PathBuf> {
    Ok(preview_ingress_route_file_in_root(&homeboy()?, id))
}
