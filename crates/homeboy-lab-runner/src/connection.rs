use std::collections::BTreeMap;
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use reqwest::blocking::Client;
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

use homeboy_core::api_jobs::{
    ActiveRunnerJobSummary, JobClaimMetadata, JobEventKind, JobStatus, RemoteRunnerJobResult,
    RunnerJobSource,
};
use homeboy_core::daemon::{
    DaemonCandidateReconciliationResult, DaemonFreshnessReport, DaemonLeaselessRecoveryResult,
    DaemonStaleReasonCode, DaemonStartResult, DaemonStateLossRecoveryResult,
};
use homeboy_core::engine::shell;
use homeboy_core::error::{Error, Result};
use homeboy_core::http_api::RunSummary;
use homeboy_core::paths;
use homeboy_core::server::{self, Server, SshClient};

use super::session::{
    RunnerActiveJobError, RunnerActiveJobSource, RunnerActiveJobState, RunnerActiveJobsSnapshot,
    RunnerChangedRuntimePath, RunnerConnectReport, RunnerDisconnectReport, RunnerFailureKind,
    RunnerLeaselessRecoveryContract, RunnerLeaselessRecoveryEvidence, RunnerProxyForward,
    RunnerSession, RunnerSessionRole, RunnerSessionState, RunnerStaleDaemonWarning,
    RunnerStaleRuntimePath, RunnerStatusReport, RunnerTunnelMode, RunnerTunnelProcessStartIdentity,
};
use super::{load, remote_runner_homeboy_path, Runner, RunnerKind};

/// Collision-resistant, bounded path/unit component for controller ownership.
/// Keep this shared by every direct-SSH and service-managed daemon location.
pub(super) fn controller_scope_segment(controller_id: &str) -> String {
    let prefix = paths::sanitize_path_segment(controller_id)
        .chars()
        .take(24)
        .collect::<String>();
    let prefix = if prefix.is_empty() {
        "controller"
    } else {
        &prefix
    };
    let digest = format!("{:x}", Sha256::digest(controller_id.as_bytes()));
    format!("{prefix}-{digest}")
}

/// The one daemon state directory for a (runner, controller) pair, relative to
/// the runner user's `$HOME`. Direct SSH launches, remote daemon commands, and
/// the service unit all derive their `HOMEBOY_DAEMON_STATE_DIR` from it, so a
/// restarted daemon always reopens the job store its predecessor wrote and
/// re-adopts its jobs.
pub(super) fn daemon_state_dir(runner_id: &str, controller_id: &str) -> String {
    format!(
        ".config/homeboy/runner-daemons/{}/{}",
        paths::sanitize_path_segment(runner_id),
        controller_scope_segment(controller_id)
    )
}

const REMOTE_LEASELESS_RECOVERY_TIMEOUT: Duration = Duration::from_secs(30);
const REMOTE_RUNNER_CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
// A reader may wait for an in-flight reconnect, but never indefinitely behind a
// controller-local tunnel that disappeared during a link flap.
const CONTROLLER_PROXY_ENV: &str = "HOMEBOY_CONTROLLER_PROXY";
#[path = "connection_daemon.rs"]
mod connection_daemon;
pub(crate) use connection_daemon::versions_match;
use connection_daemon::{
    connect_remote_daemon, daemon_http_freshness_with_timeout, daemon_http_version,
};
use connection_daemon::{daemon_http_identity, normalize_homeboy_version_owned};
use connection_daemon::{
    daemon_http_version_body_with_timeout, daemon_identity_from_body,
    daemon_runtime_loaded_paths_from_body, daemon_runtime_stale_paths_from_body,
    daemon_version_from_body,
};

#[path = "connection_stop_transport_recovery.rs"]
mod stop_transport_recovery;
pub(crate) use stop_transport_recovery::{disconnect_with_session, recorded_session};

#[path = "connection_dead_lease_attestation.rs"]
mod dead_lease_attestation;
pub use dead_lease_attestation::remote_lifecycle_plan;

use super::daemon_http_get::daemon_get;

#[derive(Debug, Clone, Deserialize)]
struct CliEnvelope {
    success: bool,
    data: Option<Value>,
    error: Option<Value>,
    diagnostics: Option<Value>,
}

struct RemoteDaemonConnectOptions<'a> {
    orphan_lease_id: Option<&'a str>,
    confirmed_no_pid_job_ids: &'a [uuid::Uuid],
    reconcile_leaseless_orphans: bool,
    reconcile_unleased_candidates: bool,
    missing_lease_id: Option<&'a str>,
    recorded_pid: Option<u32>,
    recorded_endpoint: Option<&'a str>,
    live_lease_expectation: Option<(&'a str, u32)>,
}

#[derive(Debug)]
struct ControllerProxy {
    scheme: String,
    host: String,
    port: u16,
}

fn controller_proxy_from_env() -> Result<Option<ControllerProxy>> {
    let Some(value) = std::env::var(CONTROLLER_PROXY_ENV)
        .ok()
        .filter(|value| !value.trim().is_empty())
    else {
        return Ok(None);
    };
    controller_proxy_from_url(&value)
}

fn controller_proxy_from_url(value: &str) -> Result<Option<ControllerProxy>> {
    let (scheme, authority_and_path) = value.split_once("://").ok_or_else(|| {
        Error::validation_invalid_argument(
            CONTROLLER_PROXY_ENV,
            "must be an absolute proxy URL",
            None,
            None,
        )
    })?;
    if !matches!(scheme, "http" | "https" | "socks5" | "socks5h") {
        return Err(Error::validation_invalid_argument(
            CONTROLLER_PROXY_ENV,
            "must use http, https, socks5, or socks5h",
            None,
            None,
        ));
    }
    let authority = authority_and_path.split('/').next().unwrap_or_default();
    if authority.contains('@') {
        return Err(Error::validation_invalid_argument(
            CONTROLLER_PROXY_ENV,
            "must not include proxy credentials",
            None,
            Some(vec![format!(
                "Configure a credential-free controller-local proxy endpoint in {CONTROLLER_PROXY_ENV}; Homeboy never projects proxy userinfo to a runner."
            )]),
        ));
    }
    let endpoint = authority;
    let (host, port) = endpoint.rsplit_once(':').ok_or_else(|| {
        Error::validation_invalid_argument(CONTROLLER_PROXY_ENV, "must include a port", None, None)
    })?;
    let port = port.parse().map_err(|_| {
        Error::validation_invalid_argument(CONTROLLER_PROXY_ENV, "has an invalid port", None, None)
    })?;
    if host.is_empty() {
        return Err(Error::validation_invalid_argument(
            CONTROLLER_PROXY_ENV,
            "must include a host",
            None,
            None,
        ));
    }
    Ok(Some(ControllerProxy {
        scheme: scheme.to_string(),
        host: host.trim_matches(['[', ']']).to_string(),
        port,
    }))
}

fn open_controller_proxy_forward(server: &Server) -> Result<Option<RunnerProxyForward>> {
    let Some(proxy) = controller_proxy_from_env()? else {
        return Ok(None);
    };
    let mut tunnel = remote_daemon::open_reverse_proxy_tunnel(server, &proxy.host, proxy.port);
    if !tunnel.success {
        tunnel.contain_child();
        return Err(Error::validation_invalid_argument(
            "controller_proxy",
            format!(
                "controller proxy reverse forward failed: {}",
                tunnel.stderr.trim()
            ),
            None,
            None,
        ));
    }
    let tunnel_pid = tunnel.pid.expect("remote SSH forward has a child process");
    let port = tunnel.stderr.parse::<u16>().map_err(|_| {
        tunnel.contain_child();
        Error::internal_unexpected("controller proxy forward returned no allocated port")
    })?;
    let tunnel_process_start_identity = tunnel.process_start_identity.clone();
    tunnel.release_child();
    Ok(Some(RunnerProxyForward {
        runner_url: format!("{}://127.0.0.1:{port}", proxy.scheme),
        tunnel_pid,
        tunnel_process_start_identity,
    }))
}

/// Return the runner-loopback proxy URL for an explicitly requesting direct SSH
/// job. The controller endpoint is read only here and never enters job data.
pub(crate) fn controller_proxy_forward_for_job(runner_id: &str) -> Result<String> {
    crate::runner_state_lock::with_runner_registry_lock(runner_id, || {
        let session = recorded_session(runner_id)?.ok_or_else(|| {
            Error::validation_invalid_argument(
                "controller_proxy",
                "controller proxy access requires a connected direct SSH runner session",
                Some(runner_id.to_string()),
                Some(vec![format!(
                    "Run `homeboy runner connect {runner_id}` and retry."
                )]),
            )
        })?;
        if let Some(forward) = session.proxy_forward.as_ref() {
            if proxy_forward_is_owned(forward, homeboy_core::process::process_start_identity) {
                return Ok(forward.runner_url.clone());
            }
        }
        let server_id = session.server_id.as_deref().ok_or_else(|| {
            Error::validation_invalid_argument(
                "controller_proxy",
                "direct SSH runner session has no server identity",
                Some(runner_id.to_string()),
                None,
            )
        })?;
        let server = server::load(server_id)?;
        let forward = open_controller_proxy_forward(&server)?.ok_or_else(|| {
            Error::validation_invalid_argument(
                CONTROLLER_PROXY_ENV,
                "is required when a runner job requests controller proxy access",
                None,
                Some(vec![format!(
                    "Set {CONTROLLER_PROXY_ENV} on the controller and retry."
                )]),
            )
        })?;
        let mut replacement = session.clone();
        replacement.proxy_forward = Some(forward.clone());
        match replace_session_if_matches(&session, &replacement) {
            Ok(true) => {}
            Ok(false) => {
                terminate_tunnel_with_identity(
                    forward.tunnel_pid,
                    forward.tunnel_process_start_identity.as_ref(),
                    homeboy_core::process::process_start_identity,
                    terminate_pid,
                );
                return Err(Error::validation_invalid_argument(
                    "controller_proxy",
                    "runner session changed while proxy projection was being established",
                    Some(runner_id.to_string()),
                    Some(vec![
                        "Retry the runner job; Homeboy discarded the stale proxy forward."
                            .to_string(),
                    ]),
                ));
            }
            Err(error) => {
                terminate_tunnel_with_identity(
                    forward.tunnel_pid,
                    forward.tunnel_process_start_identity.as_ref(),
                    homeboy_core::process::process_start_identity,
                    terminate_pid,
                );
                return Err(error);
            }
        }
        Ok(forward.runner_url)
    })
}

fn proxy_forward_is_owned<Inspect>(forward: &RunnerProxyForward, inspect: Inspect) -> bool
where
    Inspect:
        Fn(u32) -> std::result::Result<Option<homeboy_core::process::ProcessStartIdentity>, String>,
{
    let Some(expected) = forward.tunnel_process_start_identity.as_ref() else {
        return false;
    };
    let actual = inspect(forward.tunnel_pid)
        .ok()
        .flatten()
        .map(|identity| match identity {
            homeboy_core::process::ProcessStartIdentity::Linux { starttime_ticks } => {
                RunnerTunnelProcessStartIdentity::Linux { starttime_ticks }
            }
            homeboy_core::process::ProcessStartIdentity::Macos {
                start_seconds,
                start_microseconds,
            } => RunnerTunnelProcessStartIdentity::Macos {
                start_seconds,
                start_microseconds,
            },
        });
    actual.as_ref() == Some(expected)
}

pub fn connect(runner_id: &str) -> Result<(RunnerConnectReport, i32)> {
    connect_with_orphan_adoption_and_live_lease(
        runner_id,
        RemoteDaemonConnectOptions {
            orphan_lease_id: None,
            confirmed_no_pid_job_ids: &[],
            reconcile_leaseless_orphans: false,
            reconcile_unleased_candidates: false,
            missing_lease_id: None,
            recorded_pid: None,
            recorded_endpoint: None,
            live_lease_expectation: None,
        },
    )
}

/// `homeboy runner connect`: first bring the runner's configured Homeboy
/// binary to the controller's release, then attach exactly like [`connect`]
/// on the direct or service-managed path.
///
/// Convergence installs through `refresh-homeboy`'s verified release path,
/// which takes and releases its own runtime-promotion lease and replaces the
/// daemon through its existing reconnect mechanism. Only afterwards does the
/// attach below take connect's lease, so the two never nest. Refresh's own
/// reconnect attaches through the non-converging entry points, so convergence
/// never re-enters itself. Convergence never fails connect: its outcome is
/// reported on [`RunnerConnectReport::binary_convergence`].
pub fn connect_converging_binary(runner_id: &str) -> Result<(RunnerConnectReport, i32)> {
    let convergence = binary_convergence::converge_runner_binary(
        runner_id,
        &homeboy_product_identity::build_identity(),
    );
    let (mut report, exit_code) = connect(runner_id)?;
    report.binary_convergence = Some(convergence);
    Ok((report, exit_code))
}

/// Reach a runner for placement or admission. Lives here, beside [`connect`],
/// because unlike the transport module it may start the runner's daemon.
///
///
/// A runner with a recorded current session first gets its transport back
/// through [`ensure_transport`]. Only when the recorded daemon is
/// [`TransportFailure::Unreachable`] (for example after the runner host
/// rebooted) does it fall back to the full [`connect`]. `connect` runs the
/// idempotent `ensure-running` on the one state dir: it adopts a live daemon
/// and starts a dead one, and it never restarts a live daemon in place. An SSH
/// failure (liveness unknown) or an identity mismatch (another daemon answered)
/// is the answer and never reaches `connect`. A runner with no daemon session
/// at all goes straight to `connect`.
pub(crate) fn reach_runner(runner_id: &str) -> Result<()> {
    reach_runner_with(
        runner_id,
        |runner_id| Ok(read_session(runner_id)?.is_some()),
        |runner_id| ensure_transport(runner_id, TransportLifetime::Persisted).map(|_| ()),
        connect_until_connected,
    )
}

/// [`reach_runner`] with its three effects injected, so the choice between
/// re-establishing the transport and the no-session `connect` is testable.
pub(crate) fn reach_runner_with(
    runner_id: &str,
    has_recorded_session: impl FnOnce(&str) -> Result<bool>,
    ensure: impl FnOnce(&str) -> Result<()>,
    connect: impl FnOnce(&str) -> Result<()>,
) -> Result<()> {
    if !has_recorded_session(runner_id)? {
        return connect(runner_id);
    }
    match ensure(runner_id) {
        Err(error) if TransportFailure::of(&error) == Some(TransportFailure::Unreachable) => {
            connect(runner_id)
        }
        result => result,
    }
}

fn connect_until_connected(runner_id: &str) -> Result<()> {
    let (report, exit_code) = connect(runner_id)?;
    if report.connected && exit_code == 0 {
        return Ok(());
    }
    Err(Error::validation_invalid_argument(
        "runner",
        report
            .failure_message
            .unwrap_or_else(|| "runner connect did not report a live session".to_string()),
        Some(runner_id.to_string()),
        None,
    ))
}

/// Restart the runner's daemon in place after a binary refresh: the same
/// state directory under a new lease. The caller has proven every active job
/// is re-adoptable, so the restarted daemon continues them from its job store.
/// Jobs, runs and artifacts route through the runner's one current session, so
/// publishing the restarted session lets observers follow them through the
/// new tunnel instead of reporting a lost job, a dead lease or a replaced
/// daemon.
///
/// A service-managed runner repoints its unit at `service_binary` and restarts
/// the unit; a direct runner stops the exact previous lease and starts its
/// configured binary with the same `HOMEBOY_DAEMON_STATE_DIR`.
pub(crate) fn restart_daemon_in_place(
    runner_id: &str,
    previous: &RunnerSession,
    service_binary: &str,
) -> Result<(RunnerConnectReport, i32)> {
    let previous_lease = previous
        .remote_daemon_lease_id
        .clone()
        .filter(|lease_id| !lease_id.is_empty())
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "runner",
                "the connected daemon session has no lease to restart in place",
                Some(runner_id.to_string()),
                None,
            )
        })?;
    let runner = load(runner_id)?;
    // Pollers reattach through the runner's restart recovery lock. Holding it
    // across the restart makes them wait for the restarted session instead of
    // reattaching the stopped lease and reporting the job lost.
    restart_recovery::with_restart_recovery_lock(runner_id, || {
        let (report, exit_code) = if runner.settings.service_managed {
            service::repoint_and_restart(runner_id, service_binary)?;
            terminate_tunnel_if_owned(previous);
            service::connect_service_runner(runner_id)?
        } else {
            restart_direct_daemon_in_place(&runner, previous, &previous_lease)?
        };
        if exit_code == 0 {
            let session = read_session(runner_id)?.ok_or_else(|| {
                Error::internal_unexpected(
                    "restarted daemon connected without a controller session",
                )
            })?;
            restart_recovery::rebind_restarted_lease(runner_id, &previous_lease, &session)?;
        }
        Ok((report, exit_code))
    })
}

/// Stop the exact previous lease, start the configured binary on the same
/// state directory, and reattach the controller tunnel to it.
fn restart_direct_daemon_in_place(
    runner: &Runner,
    previous: &RunnerSession,
    previous_lease: &str,
) -> Result<(RunnerConnectReport, i32)> {
    let runner_id = runner.id.as_str();
    let session_path = session_path(runner_id)?;
    let homeboy = remote_runner_homeboy_path(runner, "runner daemon restart")?;
    let Some((server_id, server, client)) = resolve_ssh_runner(runner)? else {
        return Ok(failed_connect(
            runner_id,
            session_path,
            RunnerFailureKind::SshFailure,
            "restarting a runner daemon in place requires an SSH runner".to_string(),
        ));
    };
    let identity = match remote_homeboy_identity(&client, homeboy) {
        Ok(identity) => identity,
        Err(message) => {
            return Ok(failed_connect(
                runner_id,
                session_path,
                RunnerFailureKind::MissingRemoteHomeboy,
                message,
            ))
        }
    };
    let Some(configured_identity) = identity.build_identity.clone() else {
        return Ok(failed_connect(
            runner_id,
            session_path,
            RunnerFailureKind::MissingRemoteHomeboy,
            "the runner's homeboy did not report a build identity".to_string(),
        ));
    };
    if let Err(report) = legacy_layout::move_direct_runner(&client, homeboy, runner_id) {
        return Ok(*report);
    }
    let daemon = match restart_remote_daemon_in_place(
        &client,
        homeboy,
        runner_id,
        previous_lease,
        &configured_identity,
    ) {
        Ok(daemon) => daemon,
        Err(message) => {
            return Ok(failed_connect(
                runner_id,
                session_path,
                RunnerFailureKind::DaemonStartupFailure,
                message,
            ))
        }
    };
    terminate_tunnel_if_owned(previous);
    let expected_version = daemon.version.clone().unwrap_or(identity.version.clone());
    let (local_port, tunnel_pid, tunnel_process_start_identity, local_url, daemon) =
        match connect_remote_daemon(connection_daemon::RemoteDaemonConnectRequest {
            server: &server,
            homeboy,
            daemon,
            expected_version: &expected_version,
            expected_identity: &configured_identity,
            runner_id,
            session_path: &session_path,
        }) {
            Ok(connection) => connection,
            Err(report) => return Ok(*report),
        };
    let session = RunnerSession {
        runner_id: runner_id.to_string(),
        mode: RunnerTunnelMode::DirectSsh,
        role: RunnerSessionRole::Controller,
        server_id: Some(server_id),
        controller_id: Some(controller_id()),
        broker_url: None,
        remote_daemon_address: Some(daemon.address),
        local_port: Some(local_port),
        local_url: Some(local_url),
        tunnel_pid,
        tunnel_process_start_identity,
        proxy_forward: None,
        remote_daemon_pid: daemon.pid,
        remote_daemon_lease_id: daemon.lease_id,
        homeboy_version: expected_version,
        homeboy_build_identity: Some(configured_identity),
        connected_at: Utc::now().to_rfc3339(),
        worker_identity: None,
        worker_pid: None,
        last_seen_at: None,
        leaseless_recovery_evidence: None,
    };
    if let Err(error) = write_session(&session) {
        terminate_tunnel_if_owned(&session);
        return Err(error);
    }
    super::runner_probe_gate::invalidate_runner_probes(runner_id);
    Ok((
        RunnerConnectReport {
            runner_id: runner_id.to_string(),
            mode: Some(session.mode.clone()),
            role: Some(session.role.clone()),
            connected: true,
            recorded: None,
            local_url: session.local_url.clone(),
            broker_url: None,
            controller_id: None,
            remote_daemon_address: session.remote_daemon_address.clone(),
            tunnel_pid: session.tunnel_pid,
            remote_daemon_pid: session.remote_daemon_pid,
            connection_warning: None,
            homeboy_version: Some(session.homeboy_version.clone()),
            homeboy_build_identity: session.homeboy_build_identity.clone(),
            session_path: Some(session_path.display().to_string()),
            leaseless_recovery: None,
            state_loss_recovery: None,
            leaseless_recovery_evidence: None,
            failure_kind: None,
            failure_message: None,
            failure_evidence: None,
            binary_convergence: None,
        },
        0,
    ))
}

pub fn connect_with_orphan_adoption(
    runner_id: &str,
    orphan_lease_id: Option<&str>,
    confirmed_no_pid_job_ids: &[uuid::Uuid],
    reconcile_leaseless_orphans: bool,
    missing_lease_id: Option<&str>,
    recorded_pid: Option<u32>,
    recorded_endpoint: Option<&str>,
) -> Result<(RunnerConnectReport, i32)> {
    connect_with_orphan_adoption_and_live_lease(
        runner_id,
        RemoteDaemonConnectOptions {
            orphan_lease_id,
            confirmed_no_pid_job_ids,
            reconcile_leaseless_orphans,
            reconcile_unleased_candidates: false,
            missing_lease_id,
            recorded_pid,
            recorded_endpoint,
            live_lease_expectation: None,
        },
    )
}

/// Explicitly reconcile bounded, attributed unleased daemon candidates before
/// connecting. The remote command owns preview/apply evidence and refuses any
/// ambiguous same-store process.
pub fn connect_with_unleased_candidate_reconciliation(
    runner_id: &str,
) -> Result<(RunnerConnectReport, i32)> {
    connect_with_orphan_adoption_and_live_lease(
        runner_id,
        RemoteDaemonConnectOptions {
            orphan_lease_id: None,
            confirmed_no_pid_job_ids: &[],
            reconcile_leaseless_orphans: false,
            reconcile_unleased_candidates: true,
            missing_lease_id: None,
            recorded_pid: None,
            recorded_endpoint: None,
            live_lease_expectation: None,
        },
    )
}

/// Explicitly adopt a currently healthy remote lease. The operator supplies
/// both coordinates, which are rechecked after the tunnel health probe and
/// immediately before atomic session persistence.
pub fn connect_with_live_lease_adoption(
    runner_id: &str,
    lease_id: &str,
    pid: u32,
) -> Result<(RunnerConnectReport, i32)> {
    connect_with_orphan_adoption_and_live_lease(
        runner_id,
        RemoteDaemonConnectOptions {
            orphan_lease_id: None,
            confirmed_no_pid_job_ids: &[],
            reconcile_leaseless_orphans: false,
            reconcile_unleased_candidates: false,
            missing_lease_id: None,
            recorded_pid: None,
            recorded_endpoint: None,
            live_lease_expectation: Some((lease_id, pid)),
        },
    )
}

fn connect_with_orphan_adoption_and_live_lease(
    runner_id: &str,
    options: RemoteDaemonConnectOptions<'_>,
) -> Result<(RunnerConnectReport, i32)> {
    let roots = &homeboy_core::paths::PathRoots::from_environment()?;
    let RemoteDaemonConnectOptions {
        orphan_lease_id,
        confirmed_no_pid_job_ids,
        reconcile_leaseless_orphans,
        reconcile_unleased_candidates,
        missing_lease_id,
        recorded_pid,
        recorded_endpoint,
        live_lease_expectation,
    } = options;
    // A service-managed runner owns its daemon, so attaching replaces no
    // runtime state and takes no promotion lease. Waiting on that lease would
    // block every reconnect behind any running Cook's generation pin.
    if load(runner_id)?.settings.service_managed {
        return service::connect_service_runner(runner_id);
    }
    // Reconnect replaces daemon runtime state. It shares the promotion lease
    // with binary selection so a second session cannot reconnect against a
    // different configured executable halfway through the transaction. The
    // lease lives below the injected data root, like binary selection's, so an
    // isolated reconnect never waits on the host's live Cook pins (#14362).
    let promotion_lease = homeboy_core::runtime_promotion::acquire_in_root(
        &homeboy_core::paths::runtime_promotion_dir_in_root(roots.data()),
        "runner daemon reconnect",
        runner_id.to_string(),
    )?;
    promotion_lease.assert_generation()?;
    let runner = load(runner_id)?;
    let session_path = session_path(runner_id)?;
    let homeboy = remote_runner_homeboy_path(&runner, "runner connect")?;

    let Some((server_id, server, client)) = resolve_ssh_runner(&runner)? else {
        return Ok(failed_connect(
            runner_id,
            session_path,
            RunnerFailureKind::SshFailure,
            "only SSH runners are supported by direct runner connect in this wave".to_string(),
        ));
    };

    let ssh_probe = client.execute_with_timeout("true", REMOTE_RUNNER_CONNECT_TIMEOUT);
    if !ssh_probe.success {
        let fallback = bounded_remote_failure_message("SSH connectivity check", &ssh_probe);
        let detail = if ssh_probe.stderr.trim().is_empty() {
            fallback.clone()
        } else {
            ssh_probe.stderr.clone()
        };
        return Ok(remote_connect_failure(
            runner_id,
            session_path,
            &server_id,
            &server.host,
            ssh_probe.exit_code,
            &detail,
            RunnerFailureKind::SshFailure,
            fallback,
        ));
    }

    let identity = remote_homeboy_identity(&client, homeboy);
    let Ok(identity) = identity else {
        let message = identity.err().unwrap();
        let detail = message.clone();
        return Ok(remote_connect_failure(
            runner_id,
            session_path,
            &server_id,
            &server.host,
            -1,
            &detail,
            RunnerFailureKind::MissingRemoteHomeboy,
            message,
        ));
    };
    let version = identity.version.clone();
    let daemon_recovery_capabilities = identity.daemon_recovery_capabilities.as_deref();
    // Capability negotiation is a preflight, not a mutation. In particular, a
    // non-Linux runner cannot expose pidfd-safe reconciliation and must not get
    // a replay journal merely because this connect requested the action.
    if reconcile_unleased_candidates {
        if let Err(message) =
            negotiate_unleased_candidate_reconciliation(daemon_recovery_capabilities)
        {
            return Ok(failed_connect(
                runner_id,
                session_path,
                RunnerFailureKind::RunnerCapabilityMissing,
                message,
            ));
        }
    }
    let Some(configured_build_identity) = identity.build_identity.clone() else {
        return Ok(failed_connect(
            runner_id,
            session_path,
            RunnerFailureKind::MissingRemoteHomeboy,
            unverifiable_configured_identity_message(homeboy),
        ));
    };
    // One-time move off the pre-#16752 layout, before anything starts a
    // daemon in the new state dir; see `legacy_layout`.
    if let Err(report) = legacy_layout::move_direct_runner(&client, homeboy, runner_id) {
        return Ok(*report);
    }
    // A malformed local record is not ownership evidence. It remains fail
    // closed unless an operator supplies an exact live-lease expectation.
    // The runner's one current session is the reconnect target.
    let mut previous_session = match read_session_or_live_peer(runner_id) {
        Ok(session) => session,
        Err(error) if error.code == homeboy_core::error::ErrorCode::ConfigInvalidJson => None,
        Err(error) => return Err(error),
    };
    let mut pending_replacement = replacement_journal::pending_replacement(runner_id)?;
    let mut replacement_operation_id = None;
    if let Some(pending) = pending_replacement.as_ref() {
        if let Ok(mut observed) = remote_daemon_status(&client, homeboy, runner_id) {
            probe_remote_daemon_endpoint(&client, &mut observed, Some(runner_id));
            let exact = observed.daemon.as_ref().is_some_and(|daemon| {
                daemon.lease_id == pending.remote_daemon_lease_id
                    && daemon.pid == pending.remote_daemon_pid
                    && pending.remote_daemon_address.as_deref() == Some(daemon.address.as_str())
            });
            let exact_dead =
                exact && observed.stale_reason_code == Some(DaemonStaleReasonCode::PidDead);
            let authoritatively_superseded = !exact
                && observed.fresh
                && observed.endpoint_probe_error.is_none()
                && observed.daemon.as_ref().is_some_and(|daemon| {
                    daemon
                        .lease_id
                        .as_deref()
                        .is_some_and(|lease| !lease.is_empty())
                        && daemon.pid.is_some()
                        && parse_loopback_daemon_addr(&daemon.address).is_ok()
                });
            let exact_unhealthy_idle =
                exact && observed.endpoint_probe_error.is_some() && observed.active_jobs == 0;
            if exact_dead || authoritatively_superseded || exact_unhealthy_idle {
                if exact_unhealthy_idle {
                    previous_session = Some(pending.clone());
                }
                replacement_operation_id =
                    Some(replacement_journal::retire_pending_replacement(runner_id)?);
                pending_replacement = None;
            }
        }
    }
    // This write precedes every remote mutation. A retry reuses the same key
    // even when the SSH command completed but its response was lost.
    let replacement_operation_id = match replacement_operation_id {
        Some(operation_id) => operation_id,
        None => replacement_journal::replacement_operation(runner_id)?,
    };

    let mut leaseless_recovery = None;
    let mut state_loss_recovery = None;
    // Recovery creates a new immutable daemon. Its endpoint, not the
    // previous session, is the only safe reconnect target for this attempt.
    let mut recovery_daemon = pending_replacement
        .as_ref()
        .map(remote_daemon_from_session)
        .transpose()?;
    // A journaled command means the controller crossed the remote mutation
    // boundary but did not durably receive B's coordinates. Replay it before
    // inspecting A or selecting any ordinary replacement path.
    if recovery_daemon.is_none() {
        let replay = replacement_journal::replacement_operation_replay(runner_id)?;
        // An explicit recovery intent is authoritative over the stale automatic
        // ensure-running replay it is meant to resolve. Its exact validation
        // still runs below before the replay is terminalized.
        let replay = replay.filter(|(kind, _)| {
            should_replay_pending_replacement(
                kind,
                reconcile_unleased_candidates,
                live_lease_expectation.is_some(),
            )
        });
        if let Some((kind, command)) = replay {
            let command = if kind == "ensure-running" {
                if let Err(error) = negotiate_ensure_running_operation_id(
                    Some(&replacement_operation_id),
                    daemon_recovery_capabilities,
                ) {
                    return Ok(failed_connect(
                        runner_id,
                        session_path,
                        RunnerFailureKind::DaemonStartupFailure,
                        error,
                    ));
                }
                let command = remote_daemon_ensure_running_command(
                    homeboy,
                    runner_id,
                    Some(&replacement_operation_id),
                );
                // Rebind the durable command to the verified selected binary
                // before crossing the replay mutation boundary.
                replacement_journal::record_replacement_operation_replay(
                    runner_id,
                    "ensure-running",
                    &command,
                )?;
                command
            } else {
                command
            };
            let output = fenced_remote_mutation(
                runner_id,
                "replay_replacement_operation",
                &client,
                &command,
                REMOTE_LEASELESS_RECOVERY_TIMEOUT,
            );
            if !output.success {
                if kind == "state-loss" && is_terminal_state_loss_refusal(&output) {
                    replacement_journal::retire_rejected_state_loss_replacement(
                        runner_id, &output,
                    )?;
                } else if kind == "ensure-running" && is_terminal_ensure_running_refusal(&output) {
                    replacement_journal::retire_rejected_ensure_running_replacement(
                        runner_id,
                        output.exit_code,
                        output.timed_out,
                        &output.stdout,
                        &output.stderr,
                    )?;
                }
                return Ok(failed_connect(
                    runner_id,
                    session_path,
                    RunnerFailureKind::DaemonStartupFailure,
                    format!(
                        "replay pending replacement operation `{kind}`: {}",
                        command_failure_message("remote operation failed", &output)
                    ),
                ));
            }
            let envelope = parse_envelope(&output.stdout).map_err(|error| {
                Error::internal_json(
                    error.to_string(),
                    Some("parse pending replacement replay".to_string()),
                )
            })?;
            if kind == "unleased-candidates" {
                if let Some(data) = envelope.data.as_ref() {
                    if let Ok(recovery) =
                        serde_json::from_value::<DaemonCandidateReconciliationResult>(data.clone())
                    {
                        if !recovery.applied {
                            replacement_journal::terminalize_unleased_candidate_reconciliation(
                                runner_id,
                            )?;
                            return Ok(failed_connect(
                                runner_id,
                                session_path,
                                RunnerFailureKind::DaemonStartupFailure,
                                format!(
                                    "pending unleased candidate reconciliation remained fail-closed: {}",
                                    recovery.evidence.join("; ")
                                ),
                            ));
                        }
                    }
                }
            }
            if !envelope.success {
                if kind == "state-loss" && is_terminal_state_loss_refusal(&output) {
                    replacement_journal::retire_rejected_state_loss_replacement(
                        runner_id, &output,
                    )?;
                } else if kind == "ensure-running" && is_terminal_ensure_running_refusal(&output) {
                    replacement_journal::retire_rejected_ensure_running_replacement(
                        runner_id,
                        output.exit_code,
                        output.timed_out,
                        &output.stdout,
                        &output.stderr,
                    )?;
                }
                return Ok(failed_connect(
                    runner_id,
                    session_path,
                    RunnerFailureKind::DaemonStartupFailure,
                    format!("pending replacement operation `{kind}` returned an error envelope"),
                ));
            }
            match kind.as_str() {
                "state-loss" => {
                    let recovery = decode_state_loss_recovery(envelope.data)?;
                    recovery_daemon = Some(remote_daemon_from_recovery(
                        &recovery.replacement,
                        &configured_build_identity,
                    ));
                    state_loss_recovery = Some(recovery);
                }
                "leaseless" => {
                    let recovery = decode_leaseless_recovery(envelope.data)?;
                    recovery_daemon = Some(remote_daemon_from_recovery(
                        &recovery.replacement,
                        &configured_build_identity,
                    ));
                    leaseless_recovery = Some(recovery);
                }
                "unleased-candidates" => {
                    let recovery: DaemonCandidateReconciliationResult =
                        serde_json::from_value(envelope.data.ok_or_else(|| {
                            Error::internal_unexpected(
                                "remote unleased candidate replay returned no receipt",
                            )
                        })?)
                        .map_err(|error| {
                            Error::internal_json(
                                error.to_string(),
                                Some("parse pending unleased candidate replay".to_string()),
                            )
                        })?;
                    if !recovery.applied {
                        replacement_journal::terminalize_unleased_candidate_reconciliation(
                            runner_id,
                        )?;
                        return Ok(failed_connect(
                            runner_id,
                            session_path,
                            RunnerFailureKind::DaemonStartupFailure,
                            format!(
                                "pending unleased candidate reconciliation remained fail-closed: {}",
                                recovery.evidence.join("; ")
                            ),
                        ));
                    }
                    let replacement = recovery.replacement.ok_or_else(|| {
                        Error::internal_unexpected(
                            "remote unleased candidate replay returned no replacement",
                        )
                    })?;
                    recovery_daemon = Some(remote_daemon_from_recovery(
                        &replacement,
                        &configured_build_identity,
                    ));
                }
                "ensure-running" => {
                    let receipt: DaemonStartResult =
                        serde_json::from_value(envelope.data.ok_or_else(|| {
                            Error::internal_unexpected(
                                "remote ensure-running replay returned no receipt",
                            )
                        })?)
                        .map_err(|error| {
                            Error::internal_json(
                                error.to_string(),
                                Some("parse pending ensure-running replay".to_string()),
                            )
                        })?;
                    recovery_daemon = Some(remote_daemon_from_recovery(
                        &receipt,
                        &configured_build_identity,
                    ));
                }
                _ => {
                    return Ok(failed_connect(
                        runner_id,
                        session_path,
                        RunnerFailureKind::DaemonStartupFailure,
                        "pending replacement operation has an unsupported replay kind".to_string(),
                    ))
                }
            }
        }
    }
    if recovery_daemon.is_none() {
        if let Some(lease_id) = missing_lease_id {
            let recorded_pid = recorded_pid.ok_or_else(|| {
                Error::validation_invalid_argument(
                    "recorded_pid",
                    "state-loss recovery requires a recorded PID",
                    None,
                    None,
                )
            })?;
            let recorded_endpoint = recorded_endpoint.ok_or_else(|| {
                Error::validation_invalid_argument(
                    "recorded_endpoint",
                    "state-loss recovery requires a recorded endpoint",
                    None,
                    None,
                )
            })?;
            if !homeboy_lab_contract::daemon_recovery_capability_advertised(
                daemon_recovery_capabilities,
                homeboy_lab_contract::DAEMON_RECOVERY_STATE_LOSS_CAPABILITY,
            ) {
                return Ok(failed_connect(
                    runner_id,
                    session_path,
                    RunnerFailureKind::DaemonStartupFailure,
                    "remote Homeboy must advertise the typed state-loss recovery capability"
                        .to_string(),
                ));
            }
            let command = remote_state_loss_recovery_command(
                homeboy,
                lease_id,
                recorded_pid,
                recorded_endpoint,
                &replacement_operation_id,
            );
            replacement_journal::record_replacement_operation_replay(
                runner_id,
                "state-loss",
                &command,
            )?;
            let recovery = fenced_remote_mutation(
                runner_id,
                "recover_missing_lease_state",
                &client,
                &command,
                REMOTE_LEASELESS_RECOVERY_TIMEOUT,
            );
            if !recovery.success {
                if is_terminal_state_loss_refusal(&recovery) {
                    replacement_journal::retire_rejected_state_loss_replacement(
                        runner_id, &recovery,
                    )?;
                }
                return Ok(failed_connect(
                    runner_id,
                    session_path,
                    RunnerFailureKind::DaemonStartupFailure,
                    state_loss_recovery_failure_message(&recovery),
                ));
            }
            let envelope = parse_envelope(&recovery.stdout).map_err(|error| {
                Error::internal_json(
                    error.to_string(),
                    Some("parse state-loss daemon recovery".to_string()),
                )
            })?;
            if !envelope.success {
                if is_terminal_state_loss_refusal(&recovery) {
                    replacement_journal::retire_rejected_state_loss_replacement(
                        runner_id, &recovery,
                    )?;
                }
                return Ok(failed_connect(
                    runner_id,
                    session_path,
                    RunnerFailureKind::DaemonStartupFailure,
                    "remote exact state-loss recovery returned an error envelope".to_string(),
                ));
            }
            let recovery = decode_state_loss_recovery(envelope.data)?;
            recovery_daemon = Some(remote_daemon_from_recovery(
                &recovery.replacement,
                &configured_build_identity,
            ));
            state_loss_recovery = Some(recovery);
        }
    }
    let mut recovery_evidence = None;
    if recovery_daemon.is_none() && reconcile_leaseless_orphans {
        let recovery_addr = previous_session
            .as_ref()
            .and_then(|session| session.remote_daemon_address.as_deref())
            .filter(|address| parse_loopback_daemon_addr(address).is_ok())
            .unwrap_or("127.0.0.1:0");
        let recovery = execute_remote_leaseless_recovery(daemon_recovery_capabilities, |_| {
            let command = remote_leaseless_recovery_command(
                homeboy,
                recovery_addr,
                &replacement_operation_id,
            );
            // The negotiated command is persisted before its SSH mutation.
            // A write failure aborts before the remote operation.
            if replacement_journal::record_replacement_operation_replay(
                runner_id,
                "leaseless",
                &command,
            )
            .is_err()
            {
                return homeboy_core::server::CommandOutput {
                    success: false,
                    stdout: String::new(),
                    stderr: "could not journal replacement operation".to_string(),
                    exit_code: 1,
                    timed_out: false,
                    observation: Default::default(),
                    child_resource: None,
                };
            }
            fenced_remote_mutation(
                runner_id,
                "reconcile_leaseless_orphans",
                &client,
                &command,
                REMOTE_LEASELESS_RECOVERY_TIMEOUT,
            )
        });
        let (contract, recovery) = match recovery {
            Ok(recovery) => recovery,
            Err(message) => {
                return Ok(failed_connect(
                    runner_id,
                    session_path,
                    RunnerFailureKind::DaemonStartupFailure,
                    format!("remote Homeboy {configured_build_identity}: {message}"),
                ));
            }
        };
        if !recovery.success {
            let message = leaseless_recovery_failure_message(&recovery);
            return Ok(failed_connect(
                runner_id,
                session_path,
                RunnerFailureKind::DaemonStartupFailure,
                message,
            ));
        }
        let envelope = parse_envelope(&recovery.stdout).map_err(|error| {
            Error::internal_json(
                error.to_string(),
                Some("parse lease-less daemon recovery".to_string()),
            )
        })?;
        if !envelope.success {
            return Ok(failed_connect(
                runner_id,
                session_path,
                RunnerFailureKind::DaemonStartupFailure,
                "remote lease-less daemon recovery returned an error envelope".to_string(),
            ));
        }
        let recovery = decode_leaseless_recovery(envelope.data)?;
        recovery_daemon = Some(remote_daemon_from_recovery(
            &recovery.replacement,
            &configured_build_identity,
        ));
        recovery_evidence = Some(leaseless_recovery_evidence(
            contract,
            &configured_build_identity,
            recovery.clone(),
        ));
        leaseless_recovery = Some(recovery);
    }

    if recovery_daemon.is_none() && reconcile_unleased_candidates {
        let recovery_addr = previous_session
            .as_ref()
            .and_then(|session| session.remote_daemon_address.as_deref())
            .filter(|address| parse_loopback_daemon_addr(address).is_ok())
            .unwrap_or("127.0.0.1:0");
        let command = format!(
            "{} daemon reconcile-unleased-candidates --apply --addr {} --replacement-operation-id {}",
            shell::quote_arg(homeboy),
            shell::quote_arg(recovery_addr),
            shell::quote_arg(&replacement_operation_id),
        );
        replacement_journal::record_unleased_candidate_reconciliation_replay(runner_id, &command)?;
        let output = fenced_remote_mutation(
            runner_id,
            "reconcile_unleased_candidates",
            &client,
            &command,
            REMOTE_LEASELESS_RECOVERY_TIMEOUT,
        );
        // A structured negative result is an acknowledged terminal outcome even
        // when the remote command uses a nonzero exit status for it.
        if let Some(recovery) = parse_envelope(&output.stdout)
            .ok()
            .and_then(|envelope| envelope.data)
            .and_then(|data| {
                serde_json::from_value::<DaemonCandidateReconciliationResult>(data).ok()
            })
            .filter(|recovery| !recovery.applied)
        {
            replacement_journal::terminalize_unleased_candidate_reconciliation(runner_id)?;
            return Ok(failed_connect(
                runner_id,
                session_path,
                RunnerFailureKind::DaemonStartupFailure,
                format!(
                    "remote unleased candidate reconciliation remained fail-closed: {}",
                    recovery.evidence.join("; ")
                ),
            ));
        }
        if !output.success {
            return Ok(failed_connect(
                runner_id,
                session_path,
                RunnerFailureKind::DaemonStartupFailure,
                command_failure_message("remote unleased candidate reconciliation failed", &output),
            ));
        }
        let envelope = parse_envelope(&output.stdout).map_err(|error| {
            Error::internal_json(
                error.to_string(),
                Some("parse unleased candidate reconciliation".to_string()),
            )
        })?;
        let recovery: DaemonCandidateReconciliationResult =
            serde_json::from_value(envelope.data.ok_or_else(|| {
                Error::internal_unexpected(
                    "remote unleased candidate reconciliation returned no data",
                )
            })?)
            .map_err(|error| {
                Error::internal_json(
                    error.to_string(),
                    Some("decode unleased candidate reconciliation".to_string()),
                )
            })?;
        if !envelope.success || !recovery.applied {
            if !recovery.applied {
                replacement_journal::terminalize_unleased_candidate_reconciliation(runner_id)?;
            }
            return Ok(failed_connect(
                runner_id,
                session_path,
                RunnerFailureKind::DaemonStartupFailure,
                format!(
                    "remote unleased candidate reconciliation remained fail-closed: {}",
                    recovery.evidence.join("; ")
                ),
            ));
        }
        let replacement = recovery.replacement.ok_or_else(|| {
            Error::internal_unexpected(
                "applied unleased candidate reconciliation returned no replacement",
            )
        })?;
        recovery_daemon = Some(remote_daemon_from_recovery(
            &replacement,
            &configured_build_identity,
        ));
    }

    // The remote recovery response is the first point at which B's immutable
    // coordinates exist. Journal them before opening a tunnel so interruption
    // retries can authenticate B rather than touching the prior daemon.
    if pending_replacement.is_none() {
        if let Some(daemon) = recovery_daemon.as_ref() {
            replacement_journal::record_pending_replacement(
                runner_id,
                &pending_replacement_session(
                    runner_id,
                    &server_id,
                    daemon,
                    &version,
                    &configured_build_identity,
                    recovery_evidence.as_ref(),
                )?,
            )?;
        }
    }
    let daemon = match recovery_daemon {
        Some(daemon) => Ok(daemon),
        // Normal reconnects must inspect the live daemon first: a reachable,
        // compatible daemon reattaches, while an idle stale daemon
        // follows ReplaceIdleStale instead of bypassing its fenced lifecycle.
        None => crate::admission_fence::with_admission_fence(
            runner_id,
            "ensure_remote_daemon",
            |admission_fence| {
                ensure_remote_daemon(remote_daemon::RemoteDaemonEnsureRequest {
                    client: &client,
                    homeboy,
                    runner_id,
                    previous_session: previous_session.as_ref(),
                    configured_identity: &configured_build_identity,
                    orphan_lease_id,
                    confirmed_no_pid_job_ids,
                    live_lease_expectation,
                    replacement_operation_id: Some(&replacement_operation_id),
                    admission_fence,
                    registry_lock_held: false,
                    daemon_recovery_capabilities,
                })
            },
        ),
    };
    let Ok(daemon) = daemon else {
        let error = daemon.err().expect("failed daemon connection has an error");
        let classification = error.details.get("classification").and_then(Value::as_str);
        // `daemon_unleased_process_conflict` on the *first* ensure-running
        // attempt journaled the exact command this operation id names before
        // crossing the remote mutation boundary. The remote's refusal is
        // authoritative, not a lost response, so replaying that same command
        // on the next connect would refuse identically forever — the stale
        // pending replacement operation #15087 reported as never clearing.
        if classification == Some("daemon_unleased_process_conflict") {
            // Best-effort: the journal write above always precedes this exact
            // remote call, so the pending operation should match, but this
            // cleanup must never mask the real failure being reported below.
            let _ = replacement_journal::retire_rejected_ensure_running_replacement(
                runner_id,
                1,
                false,
                "",
                &error.message,
            );
        }
        let failure_evidence_ref = error
            .details
            .get("failure_evidence_ref")
            .cloned()
            .and_then(|value| serde_json::from_value(value).ok());
        let (mut report, exit_code) = failed_connect_after_recovery(
            runner_id,
            session_path,
            RunnerFailureKind::DaemonStartupFailure,
            error.message,
            leaseless_recovery,
            recovery_evidence,
        );
        if let Some(evidence) = report.failure_evidence.as_mut() {
            evidence.failure_evidence_ref = failure_evidence_ref;
            evidence.classification = error
                .details
                .get("classification")
                .and_then(Value::as_str)
                .unwrap_or("daemon_bootstrap_failure")
                .to_string();
            evidence.remote_start_command =
                Some(remote_daemon::remote_daemon_ensure_running_command(
                    homeboy,
                    runner_id,
                    Some(&replacement_operation_id),
                ));
            evidence.tunnel_state = Some("not_established".to_string());
            if let Some(action) = error
                .details
                .get("safe_next_action")
                .and_then(Value::as_str)
                .filter(|action| action.starts_with("homeboy "))
            {
                evidence.recovery_command = action.to_string();
            }
        }
        attach_state_loss_recovery(&mut report, state_loss_recovery);
        return Ok((report, exit_code));
    };

    // Ordinary ensure-running and exact-owner replacement also cross the
    // mutation boundary. Persist the returned immutable coordinates before
    // opening a tunnel so health or endpoint-identity failure can never strand
    // an unpublished daemon.
    if pending_replacement.is_none()
        && replacement_journal::replacement_operation_replay(runner_id)?.is_some()
    {
        replacement_journal::record_pending_replacement(
            runner_id,
            &pending_replacement_session(
                runner_id,
                &server_id,
                &daemon,
                &version,
                &configured_build_identity,
                recovery_evidence.as_ref(),
            )?,
        )?;
    }

    let expected_version = daemon.version.clone().unwrap_or(version.clone());
    let expected_identity = daemon
        .build_identity
        .clone()
        .unwrap_or(configured_build_identity.clone());
    let connection = connect_remote_daemon(connection_daemon::RemoteDaemonConnectRequest {
        server: &server,
        homeboy,
        daemon,
        expected_version: &expected_version,
        expected_identity: &expected_identity,
        runner_id,
        session_path: &session_path,
    });
    let (local_port, tunnel_pid, tunnel_process_start_identity, local_url, daemon) =
        match connection {
            Ok(connection) => connection,
            Err(report) => {
                let (mut report, exit_code) = *report;
                attach_state_loss_recovery(&mut report, state_loss_recovery);
                attach_leaseless_recovery(&mut report, leaseless_recovery, recovery_evidence);
                return Ok((report, exit_code));
            }
        };
    // Proxy exposure belongs to explicitly requesting jobs. Connecting a runner
    // must not capture an ambient controller endpoint or create an idle tunnel.
    let proxy_forward = None;

    let remote_daemon_lease_id = daemon.lease_id.clone();
    if let Err(error) = verify_live_lease_adoption(
        &client,
        homeboy,
        runner_id,
        live_lease_expectation,
        &daemon,
        &expected_identity,
    ) {
        return Ok(session_write_failure_report(
            runner_id,
            session_path,
            error,
            state_loss_recovery,
            tunnel_pid,
            tunnel_process_start_identity.as_ref(),
        ));
    }
    if live_lease_expectation.is_some() {
        replacement_journal::terminalize_ensure_running_replay_for_live_lease_adoption(runner_id)?;
    }
    let connection_warning = daemon
        .inspected_freshness
        .as_ref()
        .and_then(|report| stale_reattach_warning_for_report(&runner.id, report));
    let session = RunnerSession {
        runner_id: runner.id.clone(),
        mode: RunnerTunnelMode::DirectSsh,
        role: RunnerSessionRole::Controller,
        server_id: Some(server_id),
        controller_id: Some(controller_id()),
        broker_url: None,
        remote_daemon_address: Some(daemon.address),
        local_port: Some(local_port),
        local_url: Some(local_url),
        tunnel_pid,
        tunnel_process_start_identity,
        proxy_forward,
        remote_daemon_pid: daemon.pid,
        remote_daemon_lease_id,
        homeboy_version: expected_version,
        homeboy_build_identity: Some(expected_identity),
        connected_at: Utc::now().to_rfc3339(),
        worker_identity: None,
        worker_pid: None,
        last_seen_at: None,
        leaseless_recovery_evidence: recovery_evidence
            .as_ref()
            .and_then(|e| serde_json::to_value(e).ok()),
    };
    // The tunnel health check above re-proved the exact remote lease and PID.
    // Settle the replacement journal, then publish the authenticated daemon as
    // the runner's one current session.
    if let Err(error) = promotion_lease
        .assert_generation()
        .and_then(|_| replacement_journal::promote_pending_replacement(runner_id, &session))
        .and_then(|_| write_session(&session))
    {
        if let Some(proxy_forward) = &session.proxy_forward {
            terminate_tunnel_with_identity(
                proxy_forward.tunnel_pid,
                proxy_forward.tunnel_process_start_identity.as_ref(),
                homeboy_core::process::process_start_identity,
                terminate_pid,
            );
        }
        return Ok(session_write_failure_report(
            runner_id,
            session_path,
            error,
            state_loss_recovery,
            session.tunnel_pid,
            session.tunnel_process_start_identity.as_ref(),
        ));
    }

    // A newly authenticated admission owner may unblock durable Cooks. The
    // registered orchestration driver belongs to the controller process; builds
    // without agent-task support resolve this as an inert no-op.
    drop(promotion_lease);
    super::runner_probe_gate::invalidate_runner_probes(runner_id);
    Ok((
        RunnerConnectReport {
            runner_id: runner.id,
            mode: Some(session.mode.clone()),
            role: Some(session.role.clone()),
            connected: true,
            recorded: None,
            local_url: session.local_url.clone(),
            broker_url: None,
            controller_id: None,
            remote_daemon_address: session.remote_daemon_address.clone(),
            tunnel_pid: session.tunnel_pid,
            remote_daemon_pid: session.remote_daemon_pid,
            connection_warning,
            homeboy_version: Some(session.homeboy_version.clone()),
            homeboy_build_identity: session.homeboy_build_identity.clone(),
            session_path: Some(session_path.display().to_string()),
            leaseless_recovery,
            state_loss_recovery,
            leaseless_recovery_evidence: recovery_evidence,
            failure_kind: None,
            failure_message: None,
            failure_evidence: None,
            binary_convergence: None,
        },
        0,
    ))
}

fn negotiate_unleased_candidate_reconciliation(
    daemon_recovery_capabilities: Option<&[homeboy_lab_contract::LabCapabilityVersion]>,
) -> std::result::Result<(), String> {
    let advertised = homeboy_lab_contract::daemon_recovery_capability_advertised(
        daemon_recovery_capabilities,
        homeboy_lab_contract::DAEMON_RECOVERY_UNLEASED_CANDIDATES_CAPABILITY,
    );
    if !advertised {
        return Err("remote Homeboy must be upgraded: daemon reconcile-unleased-candidates does not advertise the idempotent candidate-reconciliation capability".to_string());
    }
    Ok(())
}

fn remote_daemon_from_recovery(
    replacement: &DaemonStartResult,
    configured_build_identity: &str,
) -> RemoteDaemon {
    RemoteDaemon {
        address: replacement.address.clone(),
        pid: Some(replacement.pid),
        lease_id: Some(replacement.lease_id.clone()),
        version: None,
        build_identity: Some(configured_build_identity.to_string()),
        inspected_freshness: None,
    }
}

fn remote_daemon_from_session(session: &RunnerSession) -> Result<RemoteDaemon> {
    let address = session.remote_daemon_address.clone().ok_or_else(|| {
        Error::validation_invalid_argument(
            "pending_replacement",
            "pending replacement has no address",
            None,
            None,
        )
    })?;
    let lease_id = session.remote_daemon_lease_id.clone().ok_or_else(|| {
        Error::validation_invalid_argument(
            "pending_replacement",
            "pending replacement has no lease",
            None,
            None,
        )
    })?;
    let pid = session.remote_daemon_pid.ok_or_else(|| {
        Error::validation_invalid_argument(
            "pending_replacement",
            "pending replacement has no PID",
            None,
            None,
        )
    })?;
    Ok(RemoteDaemon {
        address,
        pid: Some(pid),
        lease_id: Some(lease_id),
        version: None,
        build_identity: session.homeboy_build_identity.clone(),
        inspected_freshness: None,
    })
}

fn pending_replacement_session(
    runner_id: &str,
    server_id: &str,
    daemon: &RemoteDaemon,
    version: &str,
    identity: &str,
    recovery_evidence: Option<&RunnerLeaselessRecoveryEvidence>,
) -> Result<RunnerSession> {
    Ok(RunnerSession {
        runner_id: runner_id.to_string(),
        mode: RunnerTunnelMode::DirectSsh,
        role: RunnerSessionRole::Controller,
        server_id: Some(server_id.to_string()),
        controller_id: None,
        broker_url: None,
        remote_daemon_address: Some(daemon.address.clone()),
        local_port: None,
        local_url: None,
        tunnel_pid: None,
        tunnel_process_start_identity: None,
        proxy_forward: None,
        remote_daemon_pid: daemon.pid,
        remote_daemon_lease_id: daemon.lease_id.clone(),
        homeboy_version: version.to_string(),
        homeboy_build_identity: Some(identity.to_string()),
        connected_at: Utc::now().to_rfc3339(),
        worker_identity: None,
        worker_pid: None,
        last_seen_at: None,
        leaseless_recovery_evidence: recovery_evidence
            .and_then(|evidence| serde_json::to_value(evidence).ok()),
    })
}

fn verify_live_lease_adoption(
    client: &SshClient,
    homeboy: &str,
    runner_id: &str,
    expectation: Option<(&str, u32)>,
    connected_daemon: &RemoteDaemon,
    expected_identity: &str,
) -> std::result::Result<(), Error> {
    let Some((expected_lease, expected_pid)) = expectation else {
        return Ok(());
    };
    let mut status =
        remote_daemon_status(client, homeboy, runner_id).map_err(Error::internal_unexpected)?;
    probe_remote_daemon_endpoint(client, &mut status, None);
    let daemon = status.daemon.ok_or_else(|| {
        Error::validation_invalid_argument(
            "adopt_live_lease",
            "live lease changed before adoption could be persisted",
            None,
            None,
        )
    })?;
    if !status.fresh
        || !status.reachable
        || status.endpoint_probe_error.is_some()
        || daemon.lease_id.as_deref() != Some(expected_lease)
        || daemon.pid != Some(expected_pid)
        || daemon.lease_id != connected_daemon.lease_id
        || daemon.pid != connected_daemon.pid
        || daemon.build_identity.as_deref().map(str::trim) != Some(expected_identity.trim())
    {
        return Err(Error::validation_invalid_argument(
            "adopt_live_lease",
            format!(
                "explicit live lease adoption snapshot no longer matches: expected lease `{expected_lease}` PID {expected_pid} build `{expected_identity}`; current lease `{}` PID `{}` build `{}`; no session state was changed",
                daemon.lease_id.as_deref().unwrap_or("unavailable"),
                daemon.pid.map(|pid| pid.to_string()).as_deref().unwrap_or("unavailable"),
                daemon.build_identity.as_deref().unwrap_or("unavailable"),
            ),
            None,
            None,
        ));
    }
    Ok(())
}

fn should_replay_pending_replacement(
    kind: &str,
    reconcile_unleased_candidates: bool,
    adopt_live_lease: bool,
) -> bool {
    kind != "ensure-running" || (!reconcile_unleased_candidates && !adopt_live_lease)
}

fn remote_leaseless_recovery_command(
    homeboy: &str,
    addr: &str,
    replacement_operation_id: &str,
) -> String {
    format!(
        "{} daemon reconcile-leaseless-orphans --replacement-operation-id {} --addr {}",
        shell::quote_arg(homeboy),
        shell::quote_arg(replacement_operation_id),
        shell::quote_arg(addr),
    )
}

fn execute_remote_leaseless_recovery<Recover>(
    daemon_recovery_capabilities: Option<&[homeboy_lab_contract::LabCapabilityVersion]>,
    recover: Recover,
) -> std::result::Result<
    (
        RunnerLeaselessRecoveryContract,
        homeboy_core::server::CommandOutput,
    ),
    String,
>
where
    Recover: FnOnce(RunnerLeaselessRecoveryContract) -> homeboy_core::server::CommandOutput,
{
    let advertised = homeboy_lab_contract::daemon_recovery_capability_advertised(
        daemon_recovery_capabilities,
        homeboy_lab_contract::DAEMON_RECOVERY_LEASELESS_CAPABILITY,
    );
    if !advertised {
        return Err("remote Homeboy must be upgraded: daemon reconcile-leaseless-orphans does not advertise the lease-less recovery contract".to_string());
    }
    let contract = RunnerLeaselessRecoveryContract::ConfirmNoDaemonOwner;
    Ok((contract.clone(), recover(contract)))
}

fn leaseless_recovery_evidence(
    contract: RunnerLeaselessRecoveryContract,
    remote_command_identity: &str,
    recovery: DaemonLeaselessRecoveryResult,
) -> RunnerLeaselessRecoveryEvidence {
    RunnerLeaselessRecoveryEvidence {
        contract,
        remote_command_identity: remote_command_identity.to_string(),
        recovery: Some(recovery),
    }
}

fn decode_leaseless_recovery(data: Option<Value>) -> Result<DaemonLeaselessRecoveryResult> {
    serde_json::from_value(data.ok_or_else(|| {
        Error::internal_unexpected("remote lease-less daemon recovery returned no data")
    })?)
    .map_err(|error| {
        Error::internal_json(
            error.to_string(),
            Some("decode lease-less daemon recovery".to_string()),
        )
    })
}

fn decode_state_loss_recovery(data: Option<Value>) -> Result<DaemonStateLossRecoveryResult> {
    serde_json::from_value(data.ok_or_else(|| {
        Error::internal_unexpected("remote exact state-loss recovery returned no data")
    })?)
    .map_err(|error| {
        Error::internal_json(
            error.to_string(),
            Some("decode exact state-loss daemon recovery".to_string()),
        )
    })
}

fn attach_state_loss_recovery(
    report: &mut RunnerConnectReport,
    recovery: Option<DaemonStateLossRecoveryResult>,
) {
    report.state_loss_recovery = recovery;
}

fn session_write_failure_report(
    runner_id: &str,
    session_path: PathBuf,
    error: Error,
    recovery: Option<DaemonStateLossRecoveryResult>,
    tunnel_pid: Option<u32>,
    tunnel_process_start_identity: Option<&RunnerTunnelProcessStartIdentity>,
) -> (RunnerConnectReport, i32) {
    if let Some(pid) = tunnel_pid {
        terminate_tunnel_if_owned_parts(pid, tunnel_process_start_identity);
    }
    let (mut report, exit_code) = failed_connect(
        runner_id,
        session_path,
        RunnerFailureKind::DaemonStartupFailure,
        format!(
            "persist runner session after daemon recovery: {}",
            error.message
        ),
    );
    attach_state_loss_recovery(&mut report, recovery);
    (report, exit_code)
}

fn leaseless_recovery_failure_message(output: &homeboy_core::server::CommandOutput) -> String {
    if output.timed_out {
        format!(
            "remote lease-less daemon recovery timed out after {}s; inspect `homeboy daemon status` on the runner before retrying",
            REMOTE_LEASELESS_RECOVERY_TIMEOUT.as_secs()
        )
    } else {
        command_failure_message("remote lease-less daemon recovery failed", output)
    }
}

fn bounded_remote_failure_message(
    operation: &str,
    output: &homeboy_core::server::CommandOutput,
) -> String {
    if output.timed_out {
        format!(
            "{operation} timed out after {}s; retry the exact runner connect command",
            REMOTE_RUNNER_CONNECT_TIMEOUT.as_secs()
        )
    } else {
        command_failure_message(&format!("{operation} failed"), output)
    }
}

fn state_loss_recovery_failure_message(output: &homeboy_core::server::CommandOutput) -> String {
    if output.timed_out {
        format!(
            "remote exact state-loss recovery timed out after {}s; inspect `homeboy daemon status` on the runner before retrying",
            REMOTE_LEASELESS_RECOVERY_TIMEOUT.as_secs()
        )
    } else {
        command_failure_message("remote exact state-loss recovery failed", output)
    }
}

fn is_terminal_state_loss_refusal(output: &homeboy_core::server::CommandOutput) -> bool {
    if output.timed_out {
        return false;
    }
    let Ok(envelope) = parse_envelope(&output.stdout) else {
        return false;
    };
    if envelope.success {
        return false;
    }
    let code = envelope
        .diagnostics
        .as_ref()
        .and_then(|diagnostics| diagnostics.get("code"))
        .or_else(|| envelope.error.as_ref().and_then(|error| error.get("code")))
        .and_then(Value::as_str);
    matches!(code, Some(code) if code.starts_with("validation.") || code.starts_with("policy."))
}

/// `daemon_unleased_process_conflict` is the remote authoritatively refusing
/// this exact journaled operation id because a live process it cannot prove
/// it owns already holds the store (see `refuse_unleased_process_conflict` in
/// homeboy-core). That is not a lost response worth replaying — the same
/// command will refuse identically forever, which is exactly how #15087's
/// stale pending replacement operation kept replaying on every subsequent
/// `runner connect`/`refresh-homeboy --reconnect`.
fn is_terminal_ensure_running_refusal(output: &homeboy_core::server::CommandOutput) -> bool {
    if output.timed_out {
        return false;
    }
    let Ok(envelope) = parse_envelope(&output.stdout) else {
        return false;
    };
    if envelope.success {
        return false;
    }
    ensure_running_refusal_classification(envelope.error.as_ref())
        == Some("daemon_unleased_process_conflict")
}

fn ensure_running_refusal_classification(error: Option<&Value>) -> Option<&str> {
    error
        .and_then(|error| error.get("details"))
        .and_then(|details| details.get("classification"))
        .and_then(Value::as_str)
}

fn remote_state_loss_recovery_command(
    homeboy: &str,
    lease_id: &str,
    recorded_pid: u32,
    recorded_endpoint: &str,
    replacement_operation_id: &str,
) -> String {
    format!(
        "{} daemon recover-missing-lease-state --lease-id {} --recorded-pid {} --recorded-endpoint {} --replacement-operation-id {} --addr 127.0.0.1:0",
        shell::quote_arg(homeboy),
        shell::quote_arg(lease_id),
        recorded_pid,
        shell::quote_arg(recorded_endpoint),
        shell::quote_arg(replacement_operation_id),
    )
}

fn fenced_remote_mutation(
    runner_id: &str,
    operation_name: &str,
    client: &SshClient,
    command: &str,
    timeout: Duration,
) -> homeboy_core::server::CommandOutput {
    match crate::admission_fence::with_admission_fence(runner_id, operation_name, |fence| {
        if let Some(fence) = fence {
            return Err(Error::validation_invalid_argument(
                    "reconnect",
                    format!(
                        "runner `{runner_id}` has {} unresolved active job(s); refusing daemon recovery before terminal job evidence is available",
                        fence.active_job_count,
                    ),
                    Some(runner_id.to_string()),
                    None,
                ));
        }
        Ok(client.execute_with_timeout(command, timeout))
    }) {
        Ok(output) => output,
        Err(error) => homeboy_core::server::CommandOutput {
            success: false,
            stdout: String::new(),
            stderr: error.message,
            exit_code: 1,
            timed_out: false,
            observation: Default::default(),
            child_resource: None,
        },
    }
}

pub fn status(runner_id: &str) -> Result<RunnerStatusReport> {
    status_until(
        runner_id,
        Instant::now() + crate::readonly_probe::readonly_probe_timeout(),
    )
}

/// Capture the runner's current session status under one caller-owned
/// deadline. Network observations must share this budget so diagnostic
/// callers retain their envelope even when individual endpoints stall.
pub(crate) fn status_until(runner_id: &str, deadline: Instant) -> Result<RunnerStatusReport> {
    let runner = load(runner_id)?;
    let session_path = session_path(runner_id)?;
    // Status is an observation path. In particular, a stale direct-SSH record
    // must be reported as disconnected rather than triggering tunnel recovery.
    // Recovery can wait on shared control-plane state and may open a tunnel, so
    // it belongs to explicit connect/admission operations instead.
    let (session, state) = read_session_for_status_until(runner_id, deadline)?;
    let connected = state == RunnerSessionState::Connected;
    let (stale_daemon, configured_job_binary_build_identity) =
        stale_daemon_warning_until(&runner, session.as_ref(), connected, deadline)?;
    let direct_observation = session
        .as_ref()
        .filter(|session| connected && session.local_url.is_some())
        .map(|session| describe_direct_runner_until(runner_id, session, deadline));
    let local_daemon_freshness = match &direct_observation {
        Some(Ok(observation)) => Some(observation.freshness.clone()),
        Some(Err(error)) => {
            let mut freshness = unavailable_recovery_freshness(runner_id, error.message.clone());
            if describe_capability_missing(error) {
                freshness.stale_reason_code = None;
                freshness.ownership_evidence = Some("runner service observation capability is unavailable; explicit upgrade requires separate ownership verification".to_string());
            }
            Some(freshness)
        }
        None => runner_daemon_freshness_until(&runner, session.as_ref(), connected, deadline)?,
    };
    let mut daemon_freshness = local_daemon_freshness
        .or_else(|| remote_daemon_recovery_freshness_until(runner_id, &runner, deadline));
    let active_job_source = session.as_ref().and_then(active_runner_job_source);
    let direct_daemon_active_jobs =
        matches!(active_job_source, Some(RunnerActiveJobSource::DirectDaemon))
            .then(|| {
                daemon_freshness
                    .as_ref()
                    .map(|freshness| freshness.active_jobs)
            })
            .flatten();
    let (active_jobs, stale_jobs, active_job_state, active_job_error) = if let Some(observation) =
        direct_observation
    {
        match observation {
            Ok(observation) => (
                observation.active_jobs,
                observation.stale_jobs,
                RunnerActiveJobState::Available,
                None,
            ),
            Err(error) => (
                Vec::new(),
                Vec::new(),
                RunnerActiveJobState::Unavailable,
                Some(RunnerActiveJobError {
                    code: if describe_capability_missing(&error) {
                        PRE_DESCRIBE_CAPABILITY_MISSING.to_string()
                    } else {
                        error.code.as_str().to_string()
                    },
                    message: error.message,
                }),
            ),
        }
    } else if connected {
        match session.as_ref() {
            Some(session) => match runner_jobs_until(runner_id, session, deadline) {
                Ok((active_jobs, mut stale_jobs)) => {
                    // `/jobs` is the daemon's typed ownership snapshot. A
                    // separate freshness read can straddle a job transition,
                    // so never turn its stale count into a phantom owner.
                    if should_infer_child_run_orphans(active_jobs.len(), direct_daemon_active_jobs)
                    {
                        stale_jobs.extend(orphaned_child_run_jobs_until(
                            runner_id,
                            session,
                            &active_jobs,
                            deadline,
                        ));
                    }
                    (
                        active_jobs,
                        stale_jobs,
                        RunnerActiveJobState::Available,
                        None,
                    )
                }
                Err(err) => (
                    Vec::new(),
                    Vec::new(),
                    RunnerActiveJobState::Unavailable,
                    Some(RunnerActiveJobError {
                        code: err.code.as_str().to_string(),
                        message: err.message,
                    }),
                ),
            },
            None => (
                Vec::new(),
                Vec::new(),
                RunnerActiveJobState::NotQueried,
                None,
            ),
        }
    } else {
        (
            Vec::new(),
            Vec::new(),
            RunnerActiveJobState::NotQueried,
            None,
        )
    };
    if let (Some(freshness), Some(_direct_daemon_active_jobs)) =
        (daemon_freshness.as_mut(), direct_daemon_active_jobs)
    {
        // The typed `/jobs` response is the inspectable ownership snapshot.
        // Reconcile a mismatched freshness read to it without cancelling any
        // known job or synthesizing an uninspectable owner.
        freshness.active_jobs = active_jobs.len();
    }
    let selected_active_job_count =
        reconciled_active_job_count(active_jobs.len(), direct_daemon_active_jobs);
    // This is strictly the selected daemon's live count.
    let active_job_count = selected_active_job_count;
    let stale_runner_job_count = stale_jobs.len();
    let active_runner_jobs = active_jobs.iter().map(Into::into).collect();
    let stale_runner_jobs = stale_jobs.iter().map(Into::into).collect();
    let status = RunnerStatusReport {
        runner_id: runner_id.to_string(),
        connected,
        state,
        session,
        stale_daemon,
        configured_job_binary_build_identity,
        daemon_freshness,
        active_jobs,
        active_runner_jobs,
        stale_runner_jobs,
        active_job_count,
        stale_runner_job_count,
        active_job_state,
        active_job_source,
        active_job_error,
        active_job_recovery_evidence: None,
        session_path: session_path.display().to_string(),
    };
    Ok(status)
}

/// Apply the bounded reconciliation that status intentionally only projects.
/// This is an explicit lifecycle command because it may persist the current
/// session's observed daemon metadata and settle its terminal jobs. The
/// returned report is a fresh postcondition observation.
pub fn reconcile_status(runner_id: &str) -> Result<RunnerStatusReport> {
    let runner = load(runner_id)?;
    let mut report = status(runner_id)?;
    // Terminal-job settlement needs a live session transport; a disconnected
    // runner reports its reconnect action from the observation instead.
    if !report.connected {
        return Ok(report);
    }
    reconcile_session_metadata_with_observed_daemon(&runner, &mut report.session, true)?;
    reconcile_terminal_jobs(runner_id)?;
    // Jobs admitted before this observation started are terminal once the
    // session's daemon reports zero active jobs.
    let observed_since_ms = Utc::now().timestamp_millis();
    let postcondition = status(runner_id)?;
    if postcondition.connected
        && postcondition.active_job_state == RunnerActiveJobState::Available
        && postcondition.active_job_count == 0
    {
        crate::runner_state_lock::with_runner_registry_lock(runner_id, || {
            crate::admission_fence::release_admitted_jobs_observed_idle_locked(
                runner_id,
                observed_since_ms,
            )
        })?;
    }
    Ok(postcondition)
}

/// The result of a reconcile that was allowed to apply the operator-attested
/// remote dead-lease recovery: the post-attestation status plus the exact
/// remote recovery that ran, if one did.
#[derive(Debug)]
pub struct RunnerReconcileWithAttestationOutcome {
    pub status: RunnerStatusReport,
    pub attested: Option<dead_lease_attestation::RemoteAttestedReconcileApplied>,
}

/// Reconcile a runner, and when the session's daemon lacks ownership evidence
/// for its active jobs, apply the operator-attested remote dead-lease recovery
/// (`daemon reconcile-dead-lease-orphans` on the runner, bound to that exact
/// lease), then re-run the reconcile (#15556).
///
/// Without the blocker this is exactly [`reconcile_status`]. A refusal from the
/// runner side — a live daemon PID, a live child, or a changed job set — is
/// surfaced verbatim and nothing is retried.
pub fn reconcile_status_with_remote_attestation(
    runner_id: &str,
) -> Result<RunnerReconcileWithAttestationOutcome> {
    let first = reconcile_status(runner_id)?;
    if !dead_lease_attestation::blocked_by_daemon_ownership_evidence(&first) {
        return Ok(RunnerReconcileWithAttestationOutcome {
            status: first,
            attested: None,
        });
    }
    let attested = dead_lease_attestation::run_attested_dead_lease_reconcile(runner_id)?;
    // The fresh observation is the authoritative postcondition: the attested
    // recovery terminalized the exact job set, so the same reconcile that was
    // blocked now reports the recovered state.
    Ok(RunnerReconcileWithAttestationOutcome {
        status: reconcile_status(runner_id)?,
        attested: Some(attested),
    })
}

/// Return the controller-side session projection with bounded liveness
/// observation, without reconnecting or reconciling the daemon.
pub fn persisted_status(runner_id: &str) -> Result<RunnerStatusReport> {
    persisted_status_until(
        runner_id,
        std::time::Instant::now() + crate::readonly_probe::readonly_probe_timeout(),
    )
}

/// Return the persisted session projection under one caller-owned deadline.
/// This is intentionally observation-only: recovery may read a stale record,
/// but every direct-session liveness probe receives only the time remaining.
pub fn persisted_status_until(
    runner_id: &str,
    deadline: std::time::Instant,
) -> Result<RunnerStatusReport> {
    let session_path = session_path(runner_id)?;
    let (session, state) = read_session_for_status_until(runner_id, deadline)?;
    Ok(RunnerStatusReport {
        runner_id: runner_id.to_string(),
        connected: state == RunnerSessionState::Connected,
        state,
        session,
        stale_daemon: None,
        configured_job_binary_build_identity: None,
        daemon_freshness: None,
        active_jobs: Vec::new(),
        active_runner_jobs: Vec::new(),
        stale_runner_jobs: Vec::new(),
        active_job_count: 0,
        stale_runner_job_count: 0,
        active_job_state: RunnerActiveJobState::NotQueried,
        active_job_source: None,
        active_job_error: None,
        active_job_recovery_evidence: None,
        session_path: session_path.display().to_string(),
    })
}

/// Bounded, observation-only doctor projection. Unlike `status`, this never
/// reconciles jobs, sessions, or tunnels, but a disconnected
/// session still receives remote lease recovery evidence when SSH is available.
pub fn diagnostic_status(runner_id: &str) -> Result<RunnerStatusReport> {
    let mut report = persisted_status(runner_id)?;
    if !report.connected {
        let runner = load(runner_id)?;
        report.daemon_freshness = remote_daemon_recovery_freshness(runner_id, &runner);
    }
    Ok(report)
}

fn capture_tunnel_process_start_identity(
    pid: Option<u32>,
) -> Result<Option<RunnerTunnelProcessStartIdentity>> {
    let Some(pid) = pid else {
        return Ok(None);
    };
    let identity = homeboy_core::process::process_start_identity(pid)
        .map_err(|error| {
            Error::internal_unexpected(format!("capture tunnel process identity: {error}"))
        })?
        .ok_or_else(|| {
            Error::internal_unexpected("new tunnel exited before its process identity was captured")
        })?;
    Ok(Some(match identity {
        homeboy_core::process::ProcessStartIdentity::Linux { starttime_ticks } => {
            RunnerTunnelProcessStartIdentity::Linux { starttime_ticks }
        }
        homeboy_core::process::ProcessStartIdentity::Macos {
            start_seconds,
            start_microseconds,
        } => RunnerTunnelProcessStartIdentity::Macos {
            start_seconds,
            start_microseconds,
        },
    }))
}

pub(crate) fn terminate_tunnel_if_owned(session: &RunnerSession) {
    if let Some(pid) = session.tunnel_pid {
        terminate_tunnel_if_owned_parts(pid, session.tunnel_process_start_identity.as_ref());
    }
    if let Some(proxy_forward) = &session.proxy_forward {
        terminate_tunnel_with_identity(
            proxy_forward.tunnel_pid,
            proxy_forward.tunnel_process_start_identity.as_ref(),
            homeboy_core::process::process_start_identity,
            terminate_pid,
        );
    }
}

pub(super) fn terminate_tunnel_if_owned_parts(
    pid: u32,
    expected_identity: Option<&RunnerTunnelProcessStartIdentity>,
) {
    terminate_tunnel_with_identity(
        pid,
        expected_identity,
        homeboy_core::process::process_start_identity,
        terminate_pid,
    );
}

fn terminate_tunnel_with_identity<Inspect, Terminate>(
    pid: u32,
    expected_identity: Option<&RunnerTunnelProcessStartIdentity>,
    inspect: Inspect,
    terminate: Terminate,
) where
    Inspect:
        Fn(u32) -> std::result::Result<Option<homeboy_core::process::ProcessStartIdentity>, String>,
    Terminate: Fn(u32),
{
    let actual_identity = inspect(pid).ok().flatten().map(|identity| match identity {
        homeboy_core::process::ProcessStartIdentity::Linux { starttime_ticks } => {
            RunnerTunnelProcessStartIdentity::Linux { starttime_ticks }
        }
        homeboy_core::process::ProcessStartIdentity::Macos {
            start_seconds,
            start_microseconds,
        } => RunnerTunnelProcessStartIdentity::Macos {
            start_seconds,
            start_microseconds,
        },
    });
    if matches!((expected_identity, actual_identity.as_ref()), (Some(expected), Some(actual)) if expected == actual)
    {
        terminate(pid);
    }
}

/// Resolve a direct-SSH session for work admission. A readiness observation can
/// become stale when its controller-owned tunnel exits before submission.
/// Admission then re-establishes only the transport to the recorded daemon
/// through [`ensure_transport`], which proves the lease and PID before it
/// replaces the local tunnel record and never restarts the daemon; see
/// [`reach_runner`].
pub(crate) fn status_for_admission(runner_id: &str) -> Result<RunnerStatusReport> {
    status_for_admission_with(runner_id, status, reach_runner)
}

/// Resolve the immutable identity of the executable that will start runner-side
/// Homeboy jobs. Lab admission uses this instead of the controller build when
/// validating a direct SSH daemon.
pub(crate) fn configured_runner_homeboy_build_identity(
    runner: &Runner,
    homeboy: &str,
) -> Result<Option<String>> {
    let Some((_server_id, _server, client)) = resolve_ssh_runner(runner)? else {
        return Ok(None);
    };

    Ok(
        bounded_remote_homeboy_identity(&client, homeboy, Some(&runner.id))
            .ok()
            .and_then(|identity| identity.build_identity),
    )
}

pub(crate) use remote_daemon::StagingIdentityScope;

pub(crate) fn configured_runner_homeboy_handshake_evidence(
    runner: &Runner,
    homeboy: &str,
) -> Result<
    Option<(
        homeboy_lab_contract::LabRuntimeIdentity,
        Vec<homeboy_lab_contract::LabCapabilityVersion>,
    )>,
> {
    let Some((_server_id, _server, client)) = resolve_ssh_runner(runner)? else {
        return Ok(None);
    };
    Ok(
        bounded_remote_homeboy_identity(&client, homeboy, Some(&runner.id))
            .ok()
            .and_then(|identity| identity.handoff),
    )
}

pub(crate) fn daemon_lab_handoff_capabilities(
    local_url: &str,
) -> std::result::Result<(Vec<homeboy_lab_contract::LabCapabilityVersion>, Option<u32>), String> {
    connection_daemon::daemon_http_lab_handoff_capabilities(local_url)
}

fn status_for_admission_with<Status, Reconnect>(
    runner_id: &str,
    mut status_fn: Status,
    mut reconnect: Reconnect,
) -> Result<RunnerStatusReport>
where
    Status: FnMut(&str) -> Result<RunnerStatusReport>,
    Reconnect: FnMut(&str) -> Result<()>,
{
    let status = status_fn(runner_id)?;
    if status.connected {
        return Ok(status);
    }

    reconnect(runner_id)?;
    status_fn(runner_id)
}

fn should_infer_child_run_orphans(
    typed_active_jobs: usize,
    direct_daemon_active_jobs: Option<usize>,
) -> bool {
    direct_daemon_active_jobs.is_none_or(|count| typed_active_jobs >= count)
}

/// Select the daemon's direct count when available. `/jobs` supplies ownership
/// details, but its controller-facing projection can lag a live daemon job.
fn reconciled_active_job_count(
    typed_job_count: usize,
    daemon_active_count: Option<usize>,
) -> usize {
    daemon_active_count.unwrap_or(typed_job_count)
}

/// Apply the shared replacement guard to an operation's captured evidence.
pub(crate) fn active_jobs_from_replacement_observation(
    report: RunnerStatusReport,
) -> Result<Vec<ActiveRunnerJobSummary>> {
    let runner_id = &report.runner_id;
    if !report.connected {
        return Ok(Vec::new());
    }
    if report.active_job_state != RunnerActiveJobState::Available {
        let mut error = Error::validation_invalid_argument(
            "reconnect",
            format!(
                "runner `{runner_id}` is connected but its active daemon jobs could not be listed; refusing to replace the daemon"
            ),
            Some(runner_id.to_string()),
            Some(vec![format!("homeboy runner status {}", shell::quote_arg(runner_id))]),
        );
        error.details["active_job_state"] = serde_json::json!(report.active_job_state);
        return Err(error);
    }
    if let Some(error) = &report.active_job_error {
        return Err(Error::validation_invalid_argument(
            "reconnect",
            format!(
                "runner `{runner_id}` has an inconsistent active daemon job view ({}); refusing to replace the daemon",
                error.message
            ),
            Some(runner_id.to_string()),
            Some(vec![format!("homeboy runner status {}", shell::quote_arg(runner_id))]),
        ));
    }
    if authoritative_zero_active_jobs(&report) {
        return Ok(Vec::new());
    }
    Ok(report.active_jobs)
}

/// A daemon freshness report is the daemon's own job count. A daemon can
/// authoritatively prove zero active jobs either by being restartable (a stale
/// daemon safe to stop/start) or by being freshly `Recoverable` — a healthy
/// remote daemon with zero authoritatively idle jobs whose controller session
/// was merely lost and can be reconnected (#8694). Both cases can safely
/// replace an unavailable typed `/jobs` view.
pub(crate) fn authoritative_zero_active_jobs(report: &RunnerStatusReport) -> bool {
    report.daemon_freshness.as_ref().is_some_and(|freshness| {
        freshness.active_jobs == 0
            && (freshness.restartable
                || freshness.recovery_evidence
                    == Some(homeboy_core::daemon::DaemonRecoveryEvidence::Recoverable))
    })
}

fn runner_daemon_freshness_until(
    runner: &Runner,
    session: Option<&RunnerSession>,
    connected: bool,
    deadline: Instant,
) -> Result<Option<homeboy_core::daemon::DaemonFreshnessReport>> {
    if !connected || runner.kind != RunnerKind::Ssh {
        return Ok(None);
    }
    let Some(session) = session else {
        return Ok(None);
    };
    let Some(local_url) = session.local_url.as_deref() else {
        return Ok(None);
    };
    let Some(timeout) = remaining_observation_budget(deadline) else {
        return Ok(None);
    };
    Ok(daemon_http_freshness_with_timeout(
        &runner.id,
        local_url,
        &session.homeboy_version,
        session.homeboy_build_identity.as_deref().unwrap_or(""),
        timeout,
    )
    .ok())
}

/// Preserve remote lease evidence when the controller-side daemon HTTP tunnel is unavailable.
fn remote_daemon_recovery_freshness(
    runner_id: &str,
    runner: &Runner,
) -> Option<DaemonFreshnessReport> {
    remote_daemon_recovery_freshness_until(
        runner_id,
        runner,
        Instant::now() + crate::readonly_probe::readonly_probe_timeout(),
    )
}

fn remote_daemon_recovery_freshness_until(
    runner_id: &str,
    runner: &Runner,
    deadline: Instant,
) -> Option<DaemonFreshnessReport> {
    if runner.kind != RunnerKind::Ssh {
        return None;
    }
    let homeboy = match remote_runner_homeboy_path(runner, "runner status") {
        Ok(homeboy) => homeboy,
        Err(error) => return Some(unavailable_recovery_freshness(runner_id, error.message)),
    };
    let (_, _, client) = match resolve_ssh_runner(runner) {
        Ok(Some(connection)) => connection,
        Ok(None) => return None,
        Err(error) => return Some(unavailable_recovery_freshness(runner_id, error.message)),
    };
    let Some(timeout) = remaining_observation_budget(deadline) else {
        return Some(unavailable_recovery_freshness(
            runner_id,
            "runner admission observation deadline exhausted before remote daemon recovery"
                .to_string(),
        ));
    };
    match remote_daemon::bounded_remote_daemon_status_with_timeout(
        &client, homeboy, runner_id, timeout,
    ) {
        Ok(mut status) => {
            if remaining_observation_budget(deadline).is_some() {
                remote_daemon::probe_remote_daemon_endpoint_until(
                    &client,
                    &mut status,
                    Some(runner_id),
                    deadline,
                );
            }
            Some(remote_daemon_recovery_freshness_from_status(
                runner_id, &status,
            ))
        }
        Err(error) => Some(unavailable_recovery_freshness(runner_id, error)),
    }
}

fn active_runner_job_source(session: &RunnerSession) -> Option<RunnerActiveJobSource> {
    if session.local_url.is_some() {
        Some(RunnerActiveJobSource::DirectDaemon)
    } else {
        None
    }
}

fn runner_jobs(
    runner_id: &str,
    session: &RunnerSession,
) -> Result<(Vec<ActiveRunnerJobSummary>, Vec<ActiveRunnerJobSummary>)> {
    runner_jobs_until(
        runner_id,
        session,
        Instant::now() + crate::readonly_probe::readonly_probe_timeout(),
    )
}

fn runner_jobs_until(
    runner_id: &str,
    session: &RunnerSession,
    deadline: Instant,
) -> Result<(Vec<ActiveRunnerJobSummary>, Vec<ActiveRunnerJobSummary>)> {
    let timeout = remaining_observation_budget(deadline).ok_or_else(|| {
        Error::internal_unexpected(
            "runner admission observation deadline exhausted before typed jobs",
        )
    })?;
    let client = Client::builder()
        .timeout(timeout)
        .build()
        .map_err(|err| Error::internal_unexpected(format!("build active job client: {err}")))?;
    runner_jobs_with_client(runner_id, session, &client, timeout)
}

struct DirectRunnerObservation {
    freshness: DaemonFreshnessReport,
    active_jobs: Vec<ActiveRunnerJobSummary>,
    stale_jobs: Vec<ActiveRunnerJobSummary>,
}

pub(crate) const PRE_DESCRIBE_CAPABILITY_MISSING: &str = "runner_observation_capability_missing";

fn describe_capability_missing(error: &Error) -> bool {
    error.code.as_str() == "validation.invalid_argument"
        && error.details["field"] == "path"
        && error.details["id"] == homeboy_runner_contract::RUNNER_API_DESCRIBE_PATH
}

/// Explicit upgrade evidence for a service which cannot yet describe itself.
/// This is never used by status or workload admission. Two owned health reads
/// bracket a complete typed job inventory; drift or uncertain work fails closed.
pub(crate) fn pre_describe_upgrade_status(
    status: &RunnerStatusReport,
) -> Result<RunnerStatusReport> {
    let session = status
        .session
        .as_ref()
        .filter(|session| session.mode == RunnerTunnelMode::DirectSsh)
        .ok_or_else(|| Error::internal_unexpected("upgrade requires a selected direct service"))?;
    let url = session
        .local_url
        .as_deref()
        .ok_or_else(|| Error::internal_unexpected("upgrade service endpoint is unavailable"))?;
    let pid = session
        .tunnel_pid
        .ok_or_else(|| Error::internal_unexpected("upgrade tunnel owner is unavailable"))?;
    let identity = session
        .tunnel_process_start_identity
        .as_ref()
        .ok_or_else(|| Error::internal_unexpected("upgrade tunnel ownership is unverified"))?;
    if !tunnel_process_is_owned_with_observation(pid, identity).0 {
        return Err(Error::internal_unexpected(
            "upgrade tunnel ownership changed",
        ));
    }
    let deadline = Instant::now() + crate::readonly_probe::readonly_probe_timeout();
    let health = |deadline| {
        let timeout = remaining_observation_budget(deadline)
            .ok_or_else(|| Error::internal_unexpected("upgrade observation deadline exhausted"))?;
        connection_daemon::daemon_health_report_with_timeout(url, timeout)
            .map_err(Error::internal_unexpected)
    };
    validate_pre_describe_upgrade_health(session, &health(deadline)?)?;
    let client = Client::builder()
        .timeout(remaining_observation_budget(deadline).ok_or_else(|| {
            Error::internal_unexpected("upgrade job observation deadline exhausted")
        })?)
        .build()
        .map_err(|error| Error::internal_unexpected(error.to_string()))?;
    let jobs = daemon_get(&client, url, "/jobs")?;
    let active: Vec<ActiveRunnerJobSummary> =
        serde_json::from_value(jobs["body"]["active_runner_jobs"].clone()).map_err(|error| {
            Error::internal_unexpected(format!("upgrade active job inventory unavailable: {error}"))
        })?;
    let stale: Vec<ActiveRunnerJobSummary> =
        serde_json::from_value(jobs["body"]["stale_runner_jobs"].clone()).map_err(|error| {
            Error::internal_unexpected(format!("upgrade stale job inventory unavailable: {error}"))
        })?;
    if !active.is_empty() || !stale.is_empty() {
        return Err(Error::validation_invalid_argument(
            "reconnect",
            "service upgrade requires a proven idle job inventory",
            Some(status.runner_id.clone()),
            None,
        ));
    }
    let after = health(deadline)?;
    validate_pre_describe_upgrade_health(session, &after)?;
    let mut upgrade = status.clone();
    upgrade.daemon_freshness = Some(after.freshness);
    upgrade.active_job_state = RunnerActiveJobState::Available;
    upgrade.active_job_count = 0;
    upgrade.active_jobs.clear();
    upgrade.active_runner_jobs.clear();
    upgrade.stale_runner_jobs.clear();
    upgrade.stale_runner_job_count = 0;
    upgrade.active_job_error = None;
    Ok(upgrade)
}

fn validate_pre_describe_upgrade_health(
    session: &RunnerSession,
    health: &connection_daemon::DaemonHealthReport,
) -> Result<()> {
    if !health.freshness.fresh
        || health.freshness.active_jobs != 0
        || session
            .remote_daemon_lease_id
            .as_ref()
            .is_none_or(String::is_empty)
        || session.remote_daemon_pid.is_none()
        || session
            .homeboy_build_identity
            .as_ref()
            .is_none_or(String::is_empty)
        || health.freshness.lease_id != session.remote_daemon_lease_id
        || health.pid != session.remote_daemon_pid
        || health.build_identity != session.homeboy_build_identity
    {
        return Err(Error::validation_invalid_argument(
            "upgrade_ownership",
            "upgrade requires the selected fresh idle service lease, PID and build",
            Some(session.runner_id.clone()),
            None,
        ));
    }
    Ok(())
}

fn describe_direct_runner_until(
    runner_id: &str,
    session: &RunnerSession,
    deadline: Instant,
) -> Result<DirectRunnerObservation> {
    let timeout = remaining_observation_budget(deadline)
        .ok_or_else(|| Error::internal_unexpected("runner observation deadline exhausted"))?;
    let client = Client::builder()
        .timeout(timeout)
        .build()
        .map_err(|error| Error::internal_unexpected(error.to_string()))?;
    let data = daemon_get(
        &client,
        session.local_url.as_deref().expect("direct endpoint"),
        homeboy_runner_contract::RUNNER_API_DESCRIBE_PATH,
    )?;
    let body = data
        .get("body")
        .ok_or_else(|| Error::internal_unexpected("runner observation missing body"))?;
    parse_direct_runner_observation(runner_id, session, body)
}

fn parse_direct_runner_observation(
    runner_id: &str,
    session: &RunnerSession,
    body: &Value,
) -> Result<DirectRunnerObservation> {
    if body["schema"] != homeboy_runner_contract::RUNNER_SERVICE_OBSERVATION_SCHEMA
        || session.remote_daemon_lease_id.as_deref() != body["lease_id"].as_str()
        || body["lease_id"].as_str().is_none_or(str::is_empty)
    {
        return Err(Error::validation_invalid_argument(
            "runner_observation",
            "runner observation schema or service lease does not match the selected endpoint",
            Some(runner_id.to_string()),
            None,
        ));
    }
    // A service owns capacity for every job on its endpoint, including generic
    // controller work with no configured runner alias. Filtering those jobs by
    // the controller's alias recreates a count/identity disagreement.
    let active_jobs: Vec<ActiveRunnerJobSummary> =
        serde_json::from_value(body["active_runner_jobs"].clone()).map_err(|error| {
            Error::internal_json(
                error.to_string(),
                Some("describe active runner jobs".to_string()),
            )
        })?;
    let stale_jobs: Vec<ActiveRunnerJobSummary> =
        serde_json::from_value(body["stale_runner_jobs"].clone()).map_err(|error| {
            Error::internal_json(
                error.to_string(),
                Some("describe stale runner jobs".to_string()),
            )
        })?;
    let freshness: DaemonFreshnessReport = serde_json::from_value(body["freshness"].clone())
        .map_err(|error| {
            Error::internal_unexpected(format!("invalid runner freshness observation: {error}"))
        })?;
    if freshness.lease_id.as_deref() != body["lease_id"].as_str()
        || body["active_runner_job_count"].as_u64() != Some(active_jobs.len() as u64)
        || freshness.active_jobs != active_jobs.len()
    {
        return Err(Error::validation_invalid_argument(
            "runner_observation",
            "runner observation count and job identities disagree",
            Some(runner_id.to_string()),
            None,
        ));
    }
    Ok(DirectRunnerObservation {
        freshness,
        active_jobs,
        stale_jobs,
    })
}

/// Probe typed jobs through a direct daemon endpoint whose ownership was
/// already verified by an operation-scoped caller.
pub(crate) fn probe_verified_direct_daemon_jobs(
    runner_id: &str,
    session: &RunnerSession,
) -> Result<(Vec<ActiveRunnerJobSummary>, Vec<ActiveRunnerJobSummary>)> {
    if session.local_url.is_none() {
        return Err(Error::validation_invalid_argument(
            "runner",
            "verified runner connect has no direct daemon endpoint for typed job admission",
            Some(runner_id.to_string()),
            None,
        ));
    }
    runner_jobs(runner_id, session)
}

/// Re-probe the typed health endpoint before a one-operation admission relies
/// on a connect result that status has not yet projected as live.
pub(crate) fn probe_verified_direct_daemon_health(
    local_url: &str,
) -> std::result::Result<connection_daemon::DaemonHealthReport, String> {
    connection_daemon::daemon_health_report_with_timeout(
        local_url,
        crate::readonly_probe::readonly_probe_timeout(),
    )
}

pub(crate) fn tunnel_process_ownership(
    pid: u32,
    identity: &RunnerTunnelProcessStartIdentity,
) -> session_store::TunnelProcessOwnership {
    session_store::tunnel_process_ownership(
        pid,
        Some(identity),
        homeboy_core::process::process_start_identity,
    )
}

pub(crate) fn tunnel_process_is_owned_with_observation(
    pid: u32,
    identity: &RunnerTunnelProcessStartIdentity,
) -> (bool, String) {
    let ownership = tunnel_process_ownership(pid, identity);
    (
        ownership == session_store::TunnelProcessOwnership::Owned,
        format!("{ownership:?}"),
    )
}

fn typed_probe_timed_out(error: &Error, started: Instant, timeout: Duration) -> bool {
    error.details["request_timeout"]
        .as_bool()
        .unwrap_or_else(|| error.message.to_ascii_lowercase().contains("timed out"))
        || started.elapsed() >= timeout
}

fn runner_jobs_with_client(
    runner_id: &str,
    session: &RunnerSession,
    client: &Client,
    timeout: Duration,
) -> Result<(Vec<ActiveRunnerJobSummary>, Vec<ActiveRunnerJobSummary>)> {
    let started = Instant::now();
    let result: Result<(Vec<ActiveRunnerJobSummary>, Vec<ActiveRunnerJobSummary>)> = (|| {
        let (body, source) = if let Some(local_url) = session.local_url.as_deref() {
            let data = daemon_get(client, local_url, "/jobs")?;
            (
                data.get("body").cloned().ok_or_else(|| {
                    Error::internal_unexpected("daemon jobs response missing data.body")
                })?,
                RunnerJobSource::Daemon,
            )
        } else {
            return Err(Error::internal_unexpected(format!(
                "runner `{runner_id}` is connected but has no active-job status endpoint"
            )));
        };
        let active_jobs = parse_runner_jobs(
            &body,
            "active_runner_jobs",
            "parse active runner jobs",
            runner_id,
            source,
        )?;
        let stale_jobs = parse_runner_jobs(
            &body,
            "stale_runner_jobs",
            "parse stale runner jobs",
            runner_id,
            source,
        )?;
        Ok((active_jobs, stale_jobs))
    })();
    homeboy_core::observation::phase_timing::finish(
        "resolve.readonly_probe.runner_typed_jobs",
        started,
        result.is_err(),
    );
    if let Err(error) = &result {
        let timed_out = typed_probe_timed_out(error, started, timeout);
        crate::readonly_probe::record_degradation(crate::readonly_probe::ReadOnlyProbeDegradation {
            probe: "runner_typed_jobs".to_string(),
            runner_id: Some(runner_id.to_string()),
            reason_code: if timed_out {
                crate::readonly_probe::REASON_PROBE_TIMEOUT
            } else {
                crate::readonly_probe::REASON_PROBE_UNAVAILABLE
            },
            timeout_seconds: if timed_out {
                timeout.as_secs().max(1)
            } else {
                0
            },
            deadline_ms: if timed_out { timeout.as_millis() } else { 0 },
            follow_up: crate::readonly_probe::runner_status_follow_up(Some(runner_id)),
            follow_up_action: None,
            detail: format!(
                "read-only typed-job probe for runner `{runner_id}` did not complete; active-job state is partial: {}",
                error.message
            ),
        });
    }
    result
}

fn orphaned_child_run_jobs_until(
    runner_id: &str,
    session: &RunnerSession,
    active_jobs: &[ActiveRunnerJobSummary],
    deadline: Instant,
) -> Vec<ActiveRunnerJobSummary> {
    let Ok(runs) = runner_running_runs_until(session, deadline) else {
        return Vec::new();
    };
    runs.into_iter()
        .filter(|run| !child_run_has_active_job(run, active_jobs))
        .map(|run| orphaned_child_run_job(runner_id, run))
        .collect()
}

fn child_run_has_active_job(run: &RunSummary, active_jobs: &[ActiveRunnerJobSummary]) -> bool {
    active_jobs.iter().any(|job| {
        job.durable_run_id.as_deref() == Some(run.id.as_str())
            || job.command.contains(run.id.as_str())
    })
}

fn runner_running_runs_until(
    session: &RunnerSession,
    deadline: Instant,
) -> Result<Vec<RunSummary>> {
    let Some(local_url) = session.local_url.as_deref() else {
        return Ok(Vec::new());
    };
    let timeout = remaining_observation_budget(deadline).ok_or_else(|| {
        Error::internal_unexpected(
            "runner admission observation deadline exhausted before running runs",
        )
    })?;
    let client = Client::builder()
        .timeout(timeout)
        .build()
        .map_err(|err| Error::internal_unexpected(format!("build runner runs client: {err}")))?;
    runner_running_runs_with_client(&session.runner_id, local_url, &client, timeout)
}

fn remaining_observation_budget(deadline: Instant) -> Option<Duration> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    (!remaining.is_zero()).then_some(remaining)
}

fn runner_running_runs_with_client(
    runner_id: &str,
    local_url: &str,
    client: &Client,
    timeout: Duration,
) -> Result<Vec<RunSummary>> {
    let started = Instant::now();
    let data = homeboy_core::observation::phase_timing::time("resolve.readonly_probe.runner_running_runs", || daemon_get(client, local_url, "/runs?status=running&limit=1000")).map_err(|error| {
        let timed_out = typed_probe_timed_out(&error, started, timeout);
        crate::readonly_probe::record_degradation(crate::readonly_probe::ReadOnlyProbeDegradation {
            probe: "runner_running_runs".to_string(),
            runner_id: Some(runner_id.to_string()),
            reason_code: if timed_out {
                crate::readonly_probe::REASON_PROBE_TIMEOUT
            } else {
                crate::readonly_probe::REASON_PROBE_UNAVAILABLE
            },
            timeout_seconds: if timed_out {
                timeout.as_secs().max(1)
            } else {
                0
            },
            deadline_ms: if timed_out { timeout.as_millis() } else { 0 },
            follow_up: crate::readonly_probe::runner_status_follow_up(Some(runner_id)),
            follow_up_action: None,
            detail: format!(
                "read-only running-runs probe for runner `{}` did not complete; orphaned child-run detection is partial: {}",
                runner_id, error.message
            ),
        });
        error
    })?;
    let runs: Vec<RunSummary> =
        serde_json::from_value(data["body"]["runs"].clone()).map_err(|err| {
            Error::internal_json(
                err.to_string(),
                Some("parse runner daemon running runs".to_string()),
            )
        })?;
    Ok(runs
        .into_iter()
        .filter(|run| !is_synthetic_active_job_run_summary(run))
        .collect())
}

fn is_synthetic_active_job_run_summary(run: &RunSummary) -> bool {
    run.status_note
        .as_deref()
        .is_some_and(|note| note.starts_with("active runner job:"))
}

fn orphaned_child_run_job(runner_id: &str, run: RunSummary) -> ActiveRunnerJobSummary {
    ActiveRunnerJobSummary {
        runner_id: runner_id.to_string(),
        job_id: format!("orphaned-child-run-{}", run.id),
        operation: "child-run".to_string(),
        source: "runner-observation".to_string(),
        kind: run.kind,
        status: JobStatus::Failed,
        command: run
            .command
            .unwrap_or_else(|| format!("homeboy runs show {}", run.id)),
        cwd: run.cwd,
        started_at_ms: rfc3339_to_ms(&run.started_at).unwrap_or(0),
        updated_at_ms: 0,
        elapsed_ms: 0,
        heartbeat_age_ms: 0,
        claim: JobClaimMetadata {
            claim_id: None,
            claimed_by_runner_id: Some(runner_id.to_string()),
            claimed_at_ms: None,
            claim_expires_at_ms: None,
        },
        claim_expires_in_ms: None,
        lifecycle: None,
        durable_run_id: Some(run.id),
        stale_reason: Some("child_run_running_without_active_runner_job".to_string()),
        active_child_count: None,
        active_cell_count: None,
        re_adoptable: false,
    }
}

fn rfc3339_to_ms(value: &str) -> Option<u64> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .and_then(|dt| u64::try_from(dt.timestamp_millis()).ok())
}

/// Parse a runner-jobs array (`key`) out of `body`, keep only jobs for
/// `runner_id`, and tag each with its originating `source`.
fn parse_runner_jobs(
    body: &Value,
    key: &str,
    error_context: &str,
    runner_id: &str,
    source: RunnerJobSource,
) -> Result<Vec<ActiveRunnerJobSummary>> {
    let jobs: Vec<ActiveRunnerJobSummary> = serde_json::from_value(
        body.get(key)
            .cloned()
            .unwrap_or_else(|| Value::Array(Vec::new())),
    )
    .map_err(|err| Error::internal_json(err.to_string(), Some(error_context.to_string())))?;
    Ok(jobs
        .into_iter()
        .filter(|job| job.runner_id == runner_id)
        .map(|mut job| {
            job.source = source.to_string();
            job
        })
        .collect())
}

/// Reconcile terminal runner jobs through the session's authoritative transport.
/// The returned body is transport-neutral so callers retain one command contract.
pub fn reconcile_terminal_jobs(runner_id: &str) -> Result<Value> {
    let report = status(runner_id)?;
    let Some(session) = report.session.filter(|_| report.connected) else {
        return Err(Error::validation_invalid_argument(
            "runner_id",
            format!("runner `{runner_id}` is not connected"),
            Some(runner_id.to_string()),
            None,
        ));
    };
    reconcile_terminal_jobs_for_session(&session, |_| {
        let data =
            crate::execution::daemon_api_post_for_session(&session, "/jobs/reconcile-terminal")?;
        crate::execution::canonical_daemon_body(&data, "direct daemon reconcile response").cloned()
    })
}

fn reconcile_terminal_jobs_for_session<T>(
    session: &RunnerSession,
    direct: impl FnOnce(&str) -> Result<T>,
) -> Result<T> {
    if let Some(local_url) = session.local_url.as_deref() {
        return direct(local_url);
    }
    Err(Error::validation_invalid_argument(
        "runner_id",
        format!(
            "runner `{}` has no authoritative job reconciliation endpoint",
            session.runner_id
        ),
        Some(session.runner_id.clone()),
        None,
    ))
}

/// Retrieve terminal job artifact bytes through the runner's active managed
/// transport. Direct sessions read the daemon's persisted job result.
pub fn runner_artifact_content(runner_id: &str, job_id: &str, artifact_id: &str) -> Result<Value> {
    let report = status(runner_id)?;
    let Some(session) = report.session.filter(|_| report.connected) else {
        return Err(Error::validation_invalid_argument(
            "runner_id",
            format!("runner `{runner_id}` is not connected"),
            Some(runner_id.to_string()),
            None,
        ));
    };
    ensure_artifact_content_endpoint(&session)?;
    direct_daemon_job_artifact_content(runner_id, job_id, artifact_id)
}

fn ensure_artifact_content_endpoint(session: &RunnerSession) -> Result<()> {
    if session.local_url.is_some() {
        return Ok(());
    }
    Err(Error::validation_invalid_argument(
            "runner_id",
            format!(
                "direct runner `{}` has no managed daemon endpoint for artifact retrieval",
                session.runner_id
            ),
            Some(session.runner_id.clone()),
            Some(vec![
                "Reconnect the direct runner so its local daemon URL is available, then retry the review or promotion."
                    .to_string(),
            ]),
        ))
}

fn direct_daemon_job_artifact_content(
    runner_id: &str,
    job_id: &str,
    artifact_id: &str,
) -> Result<Value> {
    let snapshot = super::runner_job_log_snapshot(runner_id, job_id)?;
    let result = snapshot
        .events
        .iter()
        .rev()
        .find(|event| event.kind == JobEventKind::Result)
        .and_then(|event| event.data.clone())
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "artifact_id",
                format!("runner job `{job_id}` has no terminal result containing artifact bytes"),
                Some(artifact_id.to_string()),
                None,
            )
        })?;
    let result: RemoteRunnerJobResult = serde_json::from_value(result).map_err(|error| {
        Error::internal_json(
            error.to_string(),
            Some("parse direct runner terminal job result for artifact retrieval".to_string()),
        )
    })?;
    let artifact = result
        .artifacts
        .iter()
        .chain(result.artifact_refs.iter())
        .find(|artifact| artifact.id == artifact_id)
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "artifact_id",
                format!("direct runner job `{job_id}` has no artifact `{artifact_id}`"),
                Some(artifact_id.to_string()),
                None,
            )
        })?;
    let content_base64 = artifact.content_base64.clone().ok_or_else(|| {
        Error::validation_invalid_argument(
            "artifact_id",
            format!(
                "direct runner job `{job_id}` did not retain bytes for artifact `{artifact_id}`"
            ),
            Some(artifact_id.to_string()),
            Some(vec![
                "Only artifacts mirrored in the direct runner terminal result can be materialized."
                    .to_string(),
            ]),
        )
    })?;
    Ok(serde_json::json!({
        "command": "runner.jobs.artifacts.direct_daemon_content",
        "job_id": job_id,
        "artifact_id": artifact_id,
        "content_base64": content_base64,
        "filename": artifact.name,
        "mime": artifact.mime,
        "size_bytes": artifact.size_bytes,
        "sha256": artifact.sha256,
    }))
}

pub(crate) fn daemon_endpoint_identity(local_url: &str) -> std::result::Result<String, String> {
    daemon_http_identity(local_url)
}

fn stale_daemon_warning_until(
    runner: &Runner,
    session: Option<&RunnerSession>,
    connected: bool,
    deadline: Instant,
) -> Result<(Option<RunnerStaleDaemonWarning>, Option<String>)> {
    if !connected {
        return Ok((None, None));
    }
    let Some(session) = session else {
        return Ok((None, None));
    };
    if runner.kind != RunnerKind::Ssh {
        return Ok((None, None));
    }
    let homeboy = remote_runner_homeboy_path(runner, "runner status stale-daemon diagnostics")?;
    let Some((_server_id, _server, client)) = resolve_ssh_runner(runner)? else {
        return Ok((None, None));
    };
    // A failed probe read nothing, so there is nothing to compare. Substituting
    // the session's own version here made `versions_match` trivially true and
    // published a fabricated "current version" alongside a vague unverifiable
    // message, hiding the actual SSH failure (#11106).
    let current_identity = match remote_daemon::bounded_remote_homeboy_identity_until(
        &client,
        homeboy,
        Some(&runner.id),
        deadline,
    ) {
        Ok(identity) => identity,
        Err(probe_error) => {
            return Ok((
                Some(RunnerStaleDaemonWarning::probe_failed(
                    &runner.id,
                    session.homeboy_version.clone(),
                    session.homeboy_build_identity.clone(),
                    homeboy,
                    probe_error,
                )),
                None,
            ));
        }
    };
    let current_version = current_identity.version.clone();
    let configured_job_binary_build_identity = current_identity.build_identity.clone();
    let daemon_body = session.local_url.as_deref().and_then(|local_url| {
        remaining_observation_budget(deadline)
            .and_then(|timeout| daemon_http_version_body_with_timeout(local_url, timeout).ok())
    });
    let observed_session_version = daemon_body
        .as_ref()
        .and_then(daemon_version_from_body)
        .map(str::to_string)
        .unwrap_or_else(|| session.homeboy_version.clone());
    let daemon_identity = daemon_body
        .as_ref()
        .and_then(daemon_identity_from_body)
        .filter(|identity| !identity.trim().is_empty())
        .map(str::to_string);
    let session_identity = daemon_identity.or_else(|| session.homeboy_build_identity.clone());
    let identity_comparison = compare_identities(
        session_identity.as_deref(),
        current_identity.build_identity.as_deref(),
    );
    let stale_runtime_paths = daemon_body
        .as_ref()
        .map(daemon_runtime_stale_paths_from_body)
        .unwrap_or_default();
    let changed_runtime_paths = daemon_body
        .as_ref()
        .map(daemon_runtime_loaded_paths_from_body)
        .map(|loaded| changed_runtime_paths(&runner.env, &loaded))
        .unwrap_or_default();
    let daemon_matches_configured = daemon_runtime_is_current(
        &observed_session_version,
        &session.homeboy_version,
        &current_version,
        identity_comparison,
        &stale_runtime_paths,
        &changed_runtime_paths,
    );
    if daemon_matches_configured {
        return Ok((None, configured_job_binary_build_identity));
    }
    Ok((
        Some(
            RunnerStaleDaemonWarning::new(
                &runner.id,
                observed_session_version,
                current_version,
                session_identity,
                current_identity.build_identity,
            )
            .with_identity_unverifiable(
                &runner.id,
                homeboy,
                identity_comparison == IdentityComparison::Unverifiable,
            )
            .with_runtime_paths(&runner.id, stale_runtime_paths, changed_runtime_paths)
            .with_persisted_session_version(&runner.id, session.homeboy_version.clone()),
        ),
        configured_job_binary_build_identity,
    ))
}

/// A daemon endpoint and configured executable jointly provide stronger evidence
/// than a session record written by a prior controller generation.
fn reconcile_session_metadata_with_observed_daemon(
    runner: &Runner,
    session: &mut Option<RunnerSession>,
    connected: bool,
) -> Result<()> {
    if !connected || runner.kind != RunnerKind::Ssh {
        return Ok(());
    }
    let Some(current_session) = session.as_mut() else {
        return Ok(());
    };
    let Some(local_url) = current_session.local_url.as_deref() else {
        return Ok(());
    };
    let homeboy = remote_runner_homeboy_path(runner, "runner status metadata reconciliation")?;
    let Some((_, _, client)) = resolve_ssh_runner(runner)? else {
        return Ok(());
    };
    let Ok(configured) = bounded_remote_homeboy_identity(&client, homeboy, Some(&runner.id)) else {
        return Ok(());
    };
    let Ok(observed_version) = daemon_http_version(local_url) else {
        return Ok(());
    };
    let Ok(observed_identity) = daemon_http_identity(local_url) else {
        return Ok(());
    };
    if !reconcile_session_metadata(
        current_session,
        &observed_version,
        &observed_identity,
        &configured,
    ) {
        return Ok(());
    }
    write_session(current_session)
}

fn reconcile_session_metadata(
    session: &mut RunnerSession,
    observed_version: &str,
    observed_identity: &str,
    configured: &RemoteHomeboyIdentity,
) -> bool {
    if !versions_match(observed_version, &configured.version)
        || compare_identities(
            Some(observed_identity),
            configured.build_identity.as_deref(),
        ) != IdentityComparison::Match
    {
        return false;
    }
    let version = normalize_homeboy_version_owned(observed_version);
    let identity = observed_identity.trim();
    let changed = session.homeboy_version != version
        || session.homeboy_build_identity.as_deref() != Some(identity);
    session.homeboy_version = version;
    session.homeboy_build_identity = Some(identity.to_string());
    changed
}

fn daemon_runtime_is_current(
    _observed_daemon_version: &str,
    _session_version: &str,
    _command_version: &str,
    _identity_comparison: IdentityComparison,
    stale_runtime_paths: &[RunnerStaleRuntimePath],
    changed_runtime_paths: &[RunnerChangedRuntimePath],
) -> bool {
    stale_runtime_paths.is_empty() && changed_runtime_paths.is_empty()
}

fn unverifiable_configured_identity_message(homeboy: &str) -> String {
    format!(
        "configured runner executable `{homeboy}` did not report an immutable build identity; run `{} self identity` on the runner and ensure it reports `git_commit` or an exact `display`, then retry. If the binary is stale, run `homeboy runner refresh-homeboy <runner-id> --reconnect`",
        shell::quote_arg(homeboy),
    )
}

fn stale_reattach_warning_for_report(
    runner_id: &str,
    report: &DaemonFreshnessReport,
) -> Option<String> {
    (!report.fresh).then(|| format!(
        "Reattached the live remote daemon lease after drift ({:?}; {}); active jobs were preserved. Continue with `homeboy runner refresh-homeboy {runner_id} --reconnect` when its work is complete.",
        report.stale_reason_code,
        report.ownership_evidence.as_deref().unwrap_or("remote status freshness drift")
    ))
}

fn changed_runtime_paths(
    runner_env: &std::collections::HashMap<String, String>,
    loaded_paths: &BTreeMap<String, String>,
) -> Vec<RunnerChangedRuntimePath> {
    let current_paths: BTreeMap<String, String> = runner_env
        .iter()
        .filter(|(name, value)| is_runtime_path_env(name) && !value.trim().is_empty())
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect();
    let mut names: std::collections::BTreeSet<String> = loaded_paths.keys().cloned().collect();
    names.extend(current_paths.keys().cloned());
    names
        .into_iter()
        .filter_map(|env| {
            let loaded_path = loaded_paths.get(&env).cloned();
            let configured_path = current_paths.get(&env).cloned();
            if loaded_path == configured_path {
                None
            } else {
                Some(RunnerChangedRuntimePath {
                    env,
                    loaded_path,
                    configured_path,
                })
            }
        })
        .collect()
}

fn is_runtime_path_env(name: &str) -> bool {
    name.starts_with("HOMEBOY_")
        && [
            "_COMPONENT_PATH",
            "_PLUGIN_PATH",
            "_PROVIDER_PATH",
            "_RUNTIME_PATH",
        ]
        .iter()
        .any(|suffix| name.ends_with(suffix))
}

pub fn statuses() -> Result<Vec<RunnerStatusReport>> {
    let mut reports = Vec::new();
    for runner in super::list()? {
        reports.push(status(&runner.id)?);
    }
    Ok(reports)
}

pub fn persisted_statuses() -> Result<Vec<RunnerStatusReport>> {
    super::list()?
        .into_iter()
        .map(|runner| persisted_status(&runner.id))
        .collect()
}

/// A latency-bounded snapshot of each runner's active jobs — a single `/jobs`
/// query on the current session, with no daemon freshness probe.
///
/// Callers that only need the current active-job view must not pay for the
/// full `status()` observation (#9522). This queries the connected session's
/// `/jobs` endpoint once per runner — bounded work per runner. Stale-job
/// detection is a reconcile concern and is intentionally omitted here.
pub fn statuses_indexed() -> Result<Vec<RunnerActiveJobsSnapshot>> {
    let mut snapshots = Vec::new();
    for runner in super::list()? {
        // Indexed inspection is read-only: reconnecting here can start SSH work
        // and makes controller discovery depend on the unhealthy runner.
        let (session, state) = read_session_for_status_until(
            &runner.id,
            Instant::now() + crate::readonly_probe::readonly_probe_timeout(),
        )?;
        let connected = state == RunnerSessionState::Connected;
        let (active_jobs, active_job_state, active_job_error) = if connected {
            match session.as_ref() {
                Some(session) => match runner_jobs(&runner.id, session) {
                    Ok((active, _stale)) => (active, RunnerActiveJobState::Available, None),
                    Err(error) => (
                        Vec::new(),
                        RunnerActiveJobState::Unavailable,
                        Some(RunnerActiveJobError {
                            code: error.code.as_str().to_string(),
                            message: error.message,
                        }),
                    ),
                },
                None => (Vec::new(), RunnerActiveJobState::NotQueried, None),
            }
        } else {
            (Vec::new(), RunnerActiveJobState::NotQueried, None)
        };
        snapshots.push(RunnerActiveJobsSnapshot {
            runner_id: runner.id,
            connected,
            active_jobs,
            active_job_state,
            active_job_error,
        });
    }
    Ok(snapshots)
}

pub fn disconnect(runner_id: &str) -> Result<RunnerDisconnectReport> {
    stop_transport_recovery::disconnect_with_force(runner_id, false)
}

pub fn disconnect_local_recovery(runner_id: &str) -> Result<RunnerDisconnectReport> {
    stop_transport_recovery::disconnect_local_recovery(runner_id)
}

#[cfg(test)]
mod status_read_purity_tests {
    #[test]
    fn status_has_no_lifecycle_mutation_path() {
        let source = include_str!("connection.rs");
        let start = source.find("pub fn status(").expect("status exists");
        let end = start
            + source[start..]
                .find("pub fn reconcile_status(")
                .expect("explicit reconciliation follows status");
        let status = &source[start..end];

        for mutation in [
            "reconcile_session_metadata_with_observed_daemon",
            "reconcile_terminal_jobs(",
            "write_session(",
            "adopt",
            "read_session_or_live_peer",
        ] {
            assert!(
                !status.contains(mutation),
                "status must not invoke lifecycle mutation `{mutation}`"
            );
        }
    }

    /// Status, persisted status and the doctor projection only observe the
    /// recorded session through the shared liveness predicate. None of them
    /// may open a tunnel or reconnect.
    #[test]
    fn status_never_opens_a_tunnel() {
        let source = include_str!("connection.rs");
        let section = |from: &str, to: &str| {
            let start = source.find(from).expect("section start exists");
            let end = start + source[start..].find(to).expect("section end exists");
            &source[start..end]
        };
        for (name, observation) in [
            (
                "status",
                section("pub fn status(", "pub fn reconcile_status("),
            ),
            (
                "persisted_status",
                section(
                    "pub fn persisted_status(",
                    "fn capture_tunnel_process_start_identity(",
                ),
            ),
        ] {
            for transport in [
                "ensure_transport",
                "reach_runner",
                "open_transport",
                "connect_remote_daemon",
                " connect(",
                "(connect(",
                "restart_daemon_in_place",
            ] {
                assert!(
                    !observation.contains(transport),
                    "{name} must not open a tunnel through `{transport}`"
                );
            }
        }
    }
}

pub(crate) mod binary_convergence;
mod legacy_layout;
pub(crate) mod remote_daemon;
mod replacement_journal;
mod restart_recovery;
mod service;
mod session_store;
mod transport;

#[cfg(test)]
pub(crate) use remote_daemon::parse_json_from_mixed_stdout;
use remote_daemon::*;
pub(crate) use service::repoint_and_restart as repoint_and_restart_runner_service;
pub use service::{
    install as install_runner_service, service_status as runner_service_status, RunnerServiceReport,
};
pub(crate) use service::{
    observe_service_units, probe_runner_binary_version,
    service_unit_name as runner_service_unit_name, service_unit_scope, ServiceUnitObservation,
};
use session_store::*;
pub(crate) use session_store::{controller_id, read_session};
pub use session_store::{peer_session_maintenance, PeerSessionMaintenanceReport};
pub use transport::{
    close_transport, ensure_transport, transport_is_live, TransportFailure, TransportLifetime,
};

#[cfg(test)]
mod tests;
