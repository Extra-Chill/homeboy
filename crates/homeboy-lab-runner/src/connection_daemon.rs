use std::path::Path;
use std::time::Duration;

use homeboy_lab_runner_contract::LabCapabilityVersion;
use reqwest::blocking::Client;
use serde::Deserialize;
use serde_json::Value;

use homeboy_core::daemon::{DaemonFreshnessReport, DaemonStaleReasonCode};
use homeboy_core::engine::shell;
use homeboy_core::server::Server;

use super::super::session::{RunnerStaleRuntimePath, RunnerTunnelProcessStartIdentity};
use super::{
    failed_connect, open_loopback_tunnel, parse_loopback_daemon_addr, reserve_loopback_port,
    wait_for_tcp, RemoteDaemon,
};
use crate::connection::remote_daemon::parse_json_from_mixed_stdout;
use crate::daemon_repair;
use crate::session::RunnerConnectFailureEvidence;
use crate::{RunnerConnectReport, RunnerFailureKind};
use std::collections::BTreeMap;

#[derive(Debug)]
struct DaemonVersionResponse {
    body: Value,
    raw_body: String,
}

#[derive(Debug)]
pub(crate) struct DaemonHealthReport {
    pub(crate) freshness: DaemonFreshnessReport,
    pub(crate) pid: Option<u32>,
    pub(crate) build_identity: Option<String>,
}

pub(super) struct RemoteDaemonConnectRequest<'a> {
    pub(super) server: &'a Server,
    pub(super) homeboy: &'a str,
    pub(super) daemon: RemoteDaemon,
    pub(super) expected_version: &'a str,
    pub(super) expected_identity: &'a str,
    pub(super) runner_id: &'a str,
    pub(super) session_path: &'a Path,
}

type RemoteDaemonConnectResult = std::result::Result<
    (
        u16,
        Option<u32>,
        Option<RunnerTunnelProcessStartIdentity>,
        String,
        RemoteDaemon,
    ),
    Box<(RunnerConnectReport, i32)>,
>;
type TunnelOpenResult = std::result::Result<
    (
        u16,
        Option<u32>,
        Option<RunnerTunnelProcessStartIdentity>,
        String,
    ),
    Box<(RunnerConnectReport, i32)>,
>;

pub(super) fn connect_remote_daemon(
    request: RemoteDaemonConnectRequest<'_>,
) -> RemoteDaemonConnectResult {
    let RemoteDaemonConnectRequest {
        server,
        homeboy,
        daemon,
        expected_version,
        expected_identity,
        runner_id,
        session_path,
    } = request;
    let failed_after_tunnel =
        |tunnel_pid: Option<u32>,
         tunnel_process_start_identity: Option<RunnerTunnelProcessStartIdentity>,
         message: String,
         durability_stage: Option<String>,
         health_attempts: Vec<String>,
         local_url: &str| {
            if let Some(pid) = tunnel_pid {
                super::terminate_tunnel_if_owned_parts(pid, tunnel_process_start_identity.as_ref());
            }
            let (mut report, exit_code) = failed_connect(
                runner_id,
                session_path.to_path_buf(),
                RunnerFailureKind::DaemonStartupFailure,
                message,
            );
            report.failure_evidence = Some(RunnerConnectFailureEvidence {
                recovery_command: format!("homeboy runner connect {}", shell::quote_arg(runner_id)),
                classification: "daemon_health".to_string(),
                remote_start_command: Some(format!(
                    "{} daemon ensure-running --addr 127.0.0.1:0",
                    shell::quote_arg(homeboy)
                )),
                known_remote_pid: daemon.pid,
                known_remote_lease_id: daemon.lease_id.clone(),
                remote_address: Some(daemon.address.clone()),
                local_address: Some(local_url.to_string()),
                tunnel_state: Some("established_then_health_failed".to_string()),
                durability_stage,
                health_attempt_count: health_attempts.len(),
                health_attempts,
                failure_evidence_ref: None,
            });
            Box::new((report, exit_code))
        };
    let (local_port, tunnel_pid, tunnel_process_start_identity, local_url) =
        open_daemon_tunnel(server, &daemon, runner_id, session_path)?;
    match probe_daemon_health_until_durable(
        &local_url,
        &daemon,
        tunnel_pid,
        tunnel_process_start_identity.as_ref(),
    ) {
        Ok(()) if endpoint_identity_matches(&local_url, expected_version, expected_identity) => {
            Ok((
                local_port,
                tunnel_pid,
                tunnel_process_start_identity,
                local_url,
                daemon,
            ))
        }
        Ok(()) => Err(failed_after_tunnel(
            tunnel_pid,
            tunnel_process_start_identity,
            "remote daemon endpoint identity did not match the controller-selected generation; refusing to publish it".to_string(),
            Some("endpoint_identity".to_string()),
            Vec::new(),
            &local_url,
        )),
        Err(failure) => {
            let durability_stage = Some(failure_stage(&failure).to_string());
            match failure {
                DaemonHealthProbeFailure::IdentityMismatch(report) => Err(failed_after_tunnel(
                    tunnel_pid,
                    tunnel_process_start_identity,
                    format!(
                        "remote daemon health identity changed or is unavailable (expected lease {:?}, PID {:?}; got lease {:?}, PID {:?}); refusing to write session{}",
                        daemon.lease_id, daemon.pid, report.freshness.lease_id, report.pid,
                        active_job_recovery_guidance(&daemon),
                    ),
                    durability_stage,
                    Vec::new(),
                    &local_url,
                )),
                DaemonHealthProbeFailure::Unreachable { message, attempts }
                | DaemonHealthProbeFailure::TunnelExited { message, attempts } => {
                    Err(failed_after_tunnel(
                        tunnel_pid,
                        tunnel_process_start_identity,
                        message,
                        durability_stage,
                        attempts,
                        &local_url,
                    ))
                }
            }
        }
    }
}

fn failure_stage(failure: &DaemonHealthProbeFailure) -> &'static str {
    match failure {
        DaemonHealthProbeFailure::IdentityMismatch(_) => "health_identity",
        DaemonHealthProbeFailure::Unreachable { .. } => "health_settle",
        DaemonHealthProbeFailure::TunnelExited { .. } => "tunnel_durability",
    }
}

fn endpoint_identity_matches(
    local_url: &str,
    expected_version: &str,
    expected_identity: &str,
) -> bool {
    let Ok(response) = daemon_http_body(local_url) else {
        return false;
    };
    let identity_matches = expected_identity.trim().is_empty()
        || daemon_identity_from_body(&response.body)
            .is_some_and(|identity| identity.trim() == expected_identity.trim());
    let version_matches = expected_version.trim().is_empty()
        || daemon_version_from_body(&response.body)
            .is_some_and(|version| versions_match(version, expected_version));
    identity_matches && version_matches
}

#[derive(Debug)]
enum DaemonHealthProbeFailure {
    /// The daemon answered but reported a different lease/PID than the one we
    /// just started. This is authoritative and must fail immediately.
    IdentityMismatch(Box<DaemonHealthReport>),
    /// The daemon endpoint could not be reached within the settle budget.
    Unreachable {
        message: String,
        attempts: Vec<String>,
    },
    /// The captured local tunnel process no longer has the exact identity that
    /// opened this connection, so it cannot be published as a live session.
    TunnelExited {
        message: String,
        attempts: Vec<String>,
    },
}

/// A freshly started daemon can have its TCP listener accepting connections
/// (so the tunnel is reachable) a beat before its HTTP handler answers
/// `/health`. A single-shot probe then fails with `error sending request` and
/// the caller rolls the whole refresh back, even though `runner connect`
/// succeeds moments later (#8459). Retry the transport-level failure within a
/// bounded settle window so the reconnect converges without operator recovery.
///
/// An identity mismatch is authoritative — the daemon is up but is the wrong
/// one — so it is never retried.
fn probe_daemon_health_until_durable(
    local_url: &str,
    daemon: &RemoteDaemon,
    tunnel_pid: Option<u32>,
    tunnel_process_start_identity: Option<&RunnerTunnelProcessStartIdentity>,
) -> std::result::Result<(), DaemonHealthProbeFailure> {
    const MAX_ATTEMPTS: usize = 4;
    const RETRY_INTERVAL: Duration = Duration::from_millis(250);
    const DURABILITY_OBSERVATIONS: usize = 2;

    let mut attempts = Vec::with_capacity(MAX_ATTEMPTS);
    for attempt in 1..=MAX_ATTEMPTS {
        match daemon_health_report(local_url) {
            Ok(report) if health_identity_matches(&report, daemon) => break,
            Ok(report) => return Err(DaemonHealthProbeFailure::IdentityMismatch(Box::new(report))),
            Err(message) => attempts.push(format!("attempt {attempt}: {message}")),
        }
        if attempt < MAX_ATTEMPTS {
            std::thread::sleep(RETRY_INTERVAL);
        }
    }
    if attempts.len() == MAX_ATTEMPTS {
        return Err(DaemonHealthProbeFailure::Unreachable {
            message: format!(
                "remote daemon health endpoint failed after {MAX_ATTEMPTS} bounded requests: {}",
                attempts
                    .last()
                    .expect("health probe records failed attempt")
            ),
            attempts,
        });
    }

    if !tunnel_process_identity_matches(tunnel_pid, tunnel_process_start_identity) {
        attempts.push(
            "durability observation 0: owned tunnel process exited or changed identity".to_string(),
        );
        return Err(DaemonHealthProbeFailure::TunnelExited {
            message: "owned SSH tunnel exited or changed identity before the post-establishment durability window".to_string(),
            attempts,
        });
    }

    // SSH may report its forward ready while its parent command is still
    // handing off. Observe beyond that window before publishing the PID.
    for observation in 1..=DURABILITY_OBSERVATIONS {
        std::thread::sleep(RETRY_INTERVAL);
        if !tunnel_process_identity_matches(tunnel_pid, tunnel_process_start_identity) {
            attempts.push(format!("durability observation {observation}: owned tunnel process exited or changed identity"));
            return Err(DaemonHealthProbeFailure::TunnelExited {
                message: "owned SSH tunnel exited or changed identity during the post-establishment durability window".to_string(),
                attempts,
            });
        }
        match daemon_health_report(local_url) {
            Ok(report) if health_identity_matches(&report, daemon) => {}
            Ok(report) => return Err(DaemonHealthProbeFailure::IdentityMismatch(Box::new(report))),
            Err(message) => {
                attempts.push(format!("durability observation {observation}: {message}"));
                if !tunnel_process_identity_matches(tunnel_pid, tunnel_process_start_identity) {
                    attempts.push(format!(
                        "durability observation {observation}: owned tunnel process exited or changed identity after health failure"
                    ));
                    return Err(DaemonHealthProbeFailure::TunnelExited {
                        message: "owned SSH tunnel exited or changed identity after a health failure during the post-establishment durability window".to_string(),
                        attempts,
                    });
                }
                return Err(DaemonHealthProbeFailure::Unreachable {
                    message: format!("remote daemon health endpoint failed during post-establishment durability observation {observation}"),
                    attempts,
                });
            }
        }
        if !tunnel_process_identity_matches(tunnel_pid, tunnel_process_start_identity) {
            attempts.push(format!("durability observation {observation}: owned tunnel process exited after health response"));
            return Err(DaemonHealthProbeFailure::TunnelExited {
                message: "owned SSH tunnel exited after a health response during the post-establishment durability window".to_string(),
                attempts,
            });
        }
    }
    Ok(())
}

fn tunnel_process_identity_matches(
    pid: Option<u32>,
    expected: Option<&RunnerTunnelProcessStartIdentity>,
) -> bool {
    match (pid, expected) {
        (Some(pid), Some(expected)) => {
            // An exited process that has not been reaped keeps its `/proc`
            // entry and its start time, so comparing start identities alone
            // reports a dead tunnel as live. Homeboy spawns the tunnel itself,
            // which makes an unreaped zombie the ordinary shape of "the tunnel
            // died" rather than an edge case: the durability window then failed
            // as `Unreachable` on the health endpoint instead of the
            // authoritative `TunnelExited`, and a session whose tunnel was
            // already gone could be published as live.
            //
            // `process_identity_state` reads the state field and reports `Z` as
            // `Dead`, which is the question actually being asked here.
            if matches!(
                homeboy_core::process::process_identity_state(pid, None),
                homeboy_core::process::ProcessIdentityState::Dead
            ) {
                return false;
            }
            super::capture_tunnel_process_start_identity(Some(pid))
                .ok()
                .flatten()
                .as_ref()
                == Some(expected)
        }
        // Loopback connections have no SSH child to own.
        (None, None) => true,
        _ => false,
    }
}

fn active_job_recovery_guidance(daemon: &RemoteDaemon) -> String {
    daemon
        .inspected_freshness
        .as_ref()
        .filter(|report| report.active_jobs > 0)
        .map(|report| format!(
            "; {} active job(s) were not replaced. Inspect `homeboy daemon status` and use explicit active-job recovery guidance before retrying",
            report.active_jobs
        ))
        .unwrap_or_default()
}

fn health_identity_matches(report: &DaemonHealthReport, daemon: &RemoteDaemon) -> bool {
    report.freshness.lease_id == daemon.lease_id
        // Older daemons did not return their PID from /health. Their live PID
        // was independently verified by bounded remote daemon status above.
        && report.pid.is_none_or(|pid| Some(pid) == daemon.pid)
}

fn open_daemon_tunnel(
    server: &Server,
    daemon: &RemoteDaemon,
    runner_id: &str,
    session_path: &Path,
) -> TunnelOpenResult {
    let Ok(remote_addr) = parse_loopback_daemon_addr(&daemon.address) else {
        return Err(Box::new(failed_connect(
            runner_id,
            session_path.to_path_buf(),
            RunnerFailureKind::DaemonStartupFailure,
            "remote daemon did not report a loopback address".to_string(),
        )));
    };

    // A loopback runner already shares the controller network namespace, so
    // its published endpoint is the reachable local endpoint. Avoid reserving
    // an unrelated port when no SSH forwarding process is needed.
    let loopback_transport = match homeboy_core::server::server_uses_loopback_transport(server) {
        Ok(loopback) => loopback,
        Err(error) => {
            return Err(Box::new(failed_connect(
                runner_id,
                session_path.to_path_buf(),
                RunnerFailureKind::TunnelFailure,
                format!("resolve SSH tunnel transport: {error}"),
            )));
        }
    };
    let local_port = if loopback_transport {
        remote_addr.port()
    } else {
        reserve_loopback_port().map_err(|err| {
            Box::new(failed_connect(
                runner_id,
                session_path.to_path_buf(),
                RunnerFailureKind::TunnelFailure,
                err.to_string(),
            ))
        })?
    };
    let mut tunnel = open_loopback_tunnel(
        server,
        local_port,
        &remote_addr.ip().to_string(),
        remote_addr.port(),
        loopback_transport,
    );
    if !tunnel.success {
        return Err(Box::new(failed_connect(
            runner_id,
            session_path.to_path_buf(),
            RunnerFailureKind::TunnelFailure,
            format!("SSH tunnel setup failed: {}", tunnel.stderr.trim()),
        )));
    }

    // Capture the local process identity before readiness can cause a fast SSH
    // child to exit and be reaped. Session cleanup relies on this exact identity.
    let tunnel_process_start_identity = tunnel.process_start_identity.clone();
    let tunnel_ready = wait_for_tcp(local_port, Duration::from_secs(5));

    if !tunnel_ready {
        tunnel.contain_child();
        return Err(Box::new(failed_connect(
            runner_id,
            session_path.to_path_buf(),
            RunnerFailureKind::TunnelFailure,
            format!(
                "local tunnel 127.0.0.1:{} did not become reachable",
                local_port
            ),
        )));
    }
    tunnel.release_child();
    Ok((
        local_port,
        tunnel.pid,
        tunnel_process_start_identity,
        format!("http://127.0.0.1:{}", local_port),
    ))
}

pub(crate) fn versions_match(left: &str, right: &str) -> bool {
    normalize_homeboy_version(left) == normalize_homeboy_version(right)
}

fn normalize_homeboy_version(version: &str) -> &str {
    version
        .trim()
        .strip_prefix("homeboy ")
        .unwrap_or(version.trim())
}

pub(super) fn normalize_homeboy_version_owned(version: &str) -> String {
    normalize_homeboy_version(version).to_string()
}

pub(super) fn daemon_http_identity(local_url: &str) -> std::result::Result<String, String> {
    let response = daemon_http_body(local_url)?;
    daemon_identity_from_body(&response.body)
        .filter(|identity| !identity.trim().is_empty())
        .map(|identity| identity.trim().to_string())
        .ok_or_else(|| {
            format!(
                "remote daemon version response did not include a build identity; raw body: {}",
                response_body_excerpt(&response.raw_body)
            )
        })
}

pub(super) fn daemon_http_version(local_url: &str) -> std::result::Result<String, String> {
    let response = daemon_http_body(local_url)?;
    daemon_version_from_body(&response.body)
        .filter(|version| !version.trim().is_empty())
        .map(|version| version.trim().to_string())
        .ok_or_else(|| {
            format!(
                "remote daemon version response did not include a version; raw body: {}",
                response_body_excerpt(&response.raw_body)
            )
        })
}

pub(crate) fn daemon_http_lab_handoff_capabilities(
    local_url: &str,
) -> std::result::Result<Vec<LabCapabilityVersion>, String> {
    let response = daemon_http_body(local_url)?;
    daemon_lab_handoff_capabilities_from_body(&response.body)
}

pub(super) fn daemon_lab_handoff_capabilities_from_body(
    body: &Value,
) -> std::result::Result<Vec<LabCapabilityVersion>, String> {
    let capabilities = body
        .get("lab_handoff_capabilities")
        .or_else(|| body.pointer("/data/lab_handoff_capabilities"))
        .ok_or_else(|| {
            "remote daemon version response did not include Lab handoff capabilities".to_string()
        })?;
    serde_json::from_value(capabilities.clone()).map_err(|_| {
        "remote daemon version response included malformed Lab handoff capabilities".to_string()
    })
}

pub(super) fn daemon_http_runtime_stale_paths_with_timeout(
    local_url: &str,
    timeout: Duration,
) -> std::result::Result<Vec<RunnerStaleRuntimePath>, String> {
    let response = daemon_http_body_at_with_timeout(local_url, "version", timeout)?;
    Ok(daemon_runtime_stale_paths_from_body(&response.body))
}

pub(super) fn daemon_http_runtime_loaded_paths_with_timeout(
    local_url: &str,
    timeout: Duration,
) -> std::result::Result<BTreeMap<String, String>, String> {
    let response = daemon_http_body_at_with_timeout(local_url, "version", timeout)?;
    Ok(daemon_runtime_loaded_paths_from_body(&response.body))
}

pub(super) fn daemon_http_version_with_timeout(
    local_url: &str,
    timeout: Duration,
) -> std::result::Result<String, String> {
    let response = daemon_http_body_at_with_timeout(local_url, "version", timeout)?;
    daemon_version_from_body(&response.body)
        .map(str::to_string)
        .ok_or_else(|| "remote daemon version response did not include a version".to_string())
}

pub(super) fn daemon_http_identity_with_timeout(
    local_url: &str,
    timeout: Duration,
) -> std::result::Result<String, String> {
    let response = daemon_http_body_at_with_timeout(local_url, "version", timeout)?;
    daemon_identity_from_body(&response.body)
        .map(str::to_string)
        .ok_or_else(|| {
            "remote daemon version response did not include a build identity".to_string()
        })
}

pub(super) fn daemon_http_freshness(
    runner_id: &str,
    local_url: &str,
    expected_version: &str,
    expected_identity: &str,
) -> std::result::Result<DaemonFreshnessReport, String> {
    daemon_http_freshness_with_timeout(
        runner_id,
        local_url,
        expected_version,
        expected_identity,
        Duration::from_secs(2),
    )
}

pub(super) fn daemon_http_freshness_with_timeout(
    runner_id: &str,
    local_url: &str,
    expected_version: &str,
    expected_identity: &str,
    timeout: Duration,
) -> std::result::Result<DaemonFreshnessReport, String> {
    daemon_freshness_report(
        runner_id,
        local_url,
        expected_version,
        expected_identity,
        timeout,
    )
}

/// A direct-SSH session is live only when its loopback endpoint still serves
/// the daemon lease recorded in the session. A listening TCP port alone can
/// belong to a replaced tunnel or an unrelated local process.
pub(super) fn daemon_http_health_matches_with_timeout(
    local_url: &str,
    expected_lease_id: Option<&str>,
    expected_pid: Option<u32>,
    timeout: Duration,
) -> bool {
    let Ok(report) = daemon_health_report_with_timeout(local_url, timeout) else {
        return false;
    };
    match expected_lease_id.filter(|lease_id| !lease_id.is_empty()) {
        Some(expected_lease_id) => {
            report.freshness.lease_id.as_deref() == Some(expected_lease_id)
                && report.pid.is_none_or(|pid| Some(pid) == expected_pid)
        }
        // Older sessions did not persist a lease. Preserve their existing
        // PID/address reattach contract rather than treating them as dead.
        None => expected_pid.is_some_and(|pid| report.pid == Some(pid)),
    }
}

fn daemon_http_body_at(
    local_url: &str,
    endpoint: &str,
) -> std::result::Result<DaemonVersionResponse, String> {
    daemon_http_body_at_with_timeout(local_url, endpoint, Duration::from_secs(2))
}

fn daemon_http_body_at_with_timeout(
    local_url: &str,
    endpoint: &str,
    timeout: Duration,
) -> std::result::Result<DaemonVersionResponse, String> {
    let client = Client::builder()
        .no_proxy()
        .timeout(timeout)
        .build()
        .map_err(|err| format!("build daemon HTTP client: {err}"))?;
    let response = client
        .get(format!("{}/{}", local_url.trim_end_matches('/'), endpoint))
        .send()
        .map_err(|err| format!("query remote daemon {endpoint}: {err}"))?;
    let status_code = response.status().as_u16();
    let body_text = response
        .text()
        .map_err(|err| format!("read remote daemon {endpoint} response: {err}"))?;
    let body: Value = parse_json_from_mixed_stdout(&body_text).map_err(|err| {
        format!(
            "parse remote daemon {endpoint} response: {err}; raw body: {}",
            response_body_excerpt(&body_text)
        )
    })?;
    if status_code >= 400 {
        return Err(format!(
            "remote daemon {endpoint} request failed with HTTP {}: {}",
            status_code, body
        ));
    }
    Ok(DaemonVersionResponse {
        body,
        raw_body: body_text,
    })
}

fn daemon_http_body(local_url: &str) -> std::result::Result<DaemonVersionResponse, String> {
    daemon_http_body_at(local_url, "version")
}

fn daemon_health_report(local_url: &str) -> std::result::Result<DaemonHealthReport, String> {
    daemon_health_report_with_timeout(local_url, Duration::from_secs(2))
}

pub(crate) fn daemon_health_report_with_timeout(
    local_url: &str,
    timeout: Duration,
) -> std::result::Result<DaemonHealthReport, String> {
    let response = daemon_http_body_at_with_timeout(local_url, "health", timeout)?;
    let freshness = daemon_freshness_from_body(&response.body).ok_or_else(|| {
        format!(
            "remote daemon health response did not include freshness; raw body: {}",
            response_body_excerpt(&response.raw_body)
        )
    })?;
    Ok(DaemonHealthReport {
        freshness,
        pid: daemon_pid_from_body(&response.body),
        build_identity: daemon_identity_from_body(&response.body).map(str::to_string),
    })
}

pub(super) fn daemon_version_from_body(body: &Value) -> Option<&str> {
    body.get("version")
        .and_then(Value::as_str)
        .or_else(|| body.pointer("/data/version").and_then(Value::as_str))
}

pub(super) fn daemon_identity_from_body(body: &Value) -> Option<&str> {
    body.pointer("/build_identity/display")
        .and_then(Value::as_str)
        .or_else(|| {
            body.pointer("/data/build_identity/display")
                .and_then(Value::as_str)
        })
}

fn response_body_excerpt(body: &str) -> String {
    const LIMIT: usize = 2000;
    let trimmed = body.trim();
    if trimmed.len() <= LIMIT {
        return trimmed.to_string();
    }
    let excerpt: String = trimmed.chars().take(LIMIT).collect();
    format!("{excerpt}...<truncated>")
}

#[derive(Debug, Deserialize)]
struct RuntimeStalePathBody {
    env: String,
    path: String,
    loaded_fingerprint: String,
    current_fingerprint: String,
}

pub(super) fn daemon_runtime_stale_paths_from_body(body: &Value) -> Vec<RunnerStaleRuntimePath> {
    let stale = body
        .pointer("/runtime_paths/stale")
        .or_else(|| body.pointer("/data/runtime_paths/stale"));
    let Some(Value::Array(paths)) = stale else {
        return Vec::new();
    };
    paths
        .iter()
        .filter_map(|value| serde_json::from_value::<RuntimeStalePathBody>(value.clone()).ok())
        .map(|path| RunnerStaleRuntimePath {
            env: path.env,
            path: path.path,
            loaded_fingerprint: path.loaded_fingerprint,
            current_fingerprint: path.current_fingerprint,
        })
        .collect()
}

pub(super) fn daemon_runtime_loaded_paths_from_body(body: &Value) -> BTreeMap<String, String> {
    let loaded = body
        .pointer("/runtime_paths/loaded")
        .or_else(|| body.pointer("/data/runtime_paths/loaded"));
    let Some(Value::Array(paths)) = loaded else {
        return BTreeMap::new();
    };
    paths
        .iter()
        .filter_map(|value| {
            Some((
                value.get("env")?.as_str()?.to_string(),
                value.get("path")?.as_str()?.to_string(),
            ))
        })
        .collect()
}

/// Restate a daemon-authored freshness report in the controller's frame.
///
/// A daemon composes its `repair_plan` and `adoption_command` as
/// `homeboy daemon *`, which is correct on the machine that runs it. Once the
/// report has crossed the SSH tunnel those commands are actively wrong: running
/// them here would stop or adopt the *controller's* daemon. The typed evidence
/// survives the crossing; the actions do not, so they are rebuilt against an
/// explicit runner id (#10302).
fn into_controller_frame(
    mut report: DaemonFreshnessReport,
    runner_id: &str,
) -> DaemonFreshnessReport {
    report.repair_plan = daemon_repair::controller_frame_plan(runner_id, &report);
    // An adoption command is one explicit, operator-confirmed action. A
    // multi-step restart sequence is a plan, not an adoption, so it is exposed
    // only as steps, and the read-only fallback is a diagnosis rather than an
    // adoption an operator could mistake for one.
    report.adoption_command = match report.repair_plan.as_slice() {
        [step] if step.code != daemon_repair::RUNNER_DIAGNOSE => Some(step.command.clone()),
        _ => None,
    };
    report
}

fn daemon_freshness_report(
    runner_id: &str,
    local_url: &str,
    expected_version: &str,
    expected_identity: &str,
    timeout: Duration,
) -> std::result::Result<DaemonFreshnessReport, String> {
    // Freshness, including the immutable executable hash, is a daemon health
    // contract. A version response is only an identity compatibility fallback.
    let DaemonVersionResponse { body, raw_body } =
        daemon_http_body_at_with_timeout(local_url, "health", timeout)?;
    if let Some(report) = daemon_freshness_from_body(&body) {
        if report.fresh
            && daemon_version_identity_mismatch(
                &body,
                &raw_body,
                expected_version,
                expected_identity,
            )?
            .is_none()
        {
            return Ok(into_controller_frame(report, runner_id));
        }
        if report.fresh {
            let mut report = report;
            report.fresh = false;
            report.stale_reason_code = Some(DaemonStaleReasonCode::VersionMismatch);
            report.restartable = true;
            return Ok(into_controller_frame(report, runner_id));
        }
        return Ok(into_controller_frame(report, runner_id));
    }
    let mismatch =
        daemon_version_identity_mismatch(&body, &raw_body, expected_version, expected_identity)?;
    Ok(into_controller_frame(
        DaemonFreshnessReport {
            fresh: mismatch.is_none(),
            stale_reason_code: mismatch.map(|_| DaemonStaleReasonCode::VersionMismatch),
            // A legacy version response has no daemon-owned job count, so it cannot
            // authorize replacement while the typed job endpoint is unavailable.
            restartable: false,
            lease_id: daemon_lease_id_from_body(&body).map(ToString::to_string),
            pid: None,
            recovery_evidence: None,
            ownership_evidence: None,
            adoption_command: None,
            binary_hash: None,
            daemon_version: daemon_version_from_body(&body).map(str::to_string),
            daemon_build_identity: daemon_identity_from_body(&body).map(str::to_string),
            runtime_paths: None,
            active_jobs: 0,
            termination_evidence: None,
            repair_plan: Vec::new(),
        },
        runner_id,
    ))
}

fn daemon_version_identity_mismatch(
    body: &Value,
    raw_body: &str,
    expected_version: &str,
    expected_identity: &str,
) -> std::result::Result<Option<String>, String> {
    if daemon_lease_id_from_body(body).is_none() {
        return Ok(Some(
            "remote daemon version response did not include a session lease".to_string(),
        ));
    }
    let running_version = daemon_version_from_body(body)
        .filter(|version| !version.trim().is_empty())
        .map(|version| version.trim().to_string())
        .ok_or_else(|| {
            format!(
                "remote daemon version response did not include a version; raw body: {}",
                response_body_excerpt(raw_body)
            )
        })?;
    if !expected_version.trim().is_empty() && !versions_match(&running_version, expected_version) {
        return Ok(Some(format!(
            "version {running_version} != configured runner version {expected_version}"
        )));
    }

    let running_identity = daemon_identity_from_body(body)
        .filter(|identity| !identity.trim().is_empty())
        .map(|identity| identity.trim().to_string())
        .ok_or_else(|| {
            format!(
                "remote daemon version response did not include a build identity; raw body: {}",
                response_body_excerpt(raw_body)
            )
        })?;
    if !versions_match(&running_identity, expected_identity) {
        return Ok(Some(format!(
            "identity {running_identity} != configured runner identity {expected_identity}"
        )));
    }

    Ok(None)
}

fn daemon_freshness_from_body(body: &Value) -> Option<DaemonFreshnessReport> {
    body.get("freshness")
        .or_else(|| body.pointer("/data/freshness"))
        .and_then(|value| serde_json::from_value(value.clone()).ok())
}

fn daemon_lease_id_from_body(body: &Value) -> Option<&str> {
    body.pointer("/lease/lease_id")
        .and_then(Value::as_str)
        .or_else(|| body.pointer("/data/lease/lease_id").and_then(Value::as_str))
}

fn daemon_pid_from_body(body: &Value) -> Option<u32> {
    body.get("pid")
        .and_then(Value::as_u64)
        .or_else(|| body.pointer("/data/pid").and_then(Value::as_u64))
        .and_then(|pid| u32::try_from(pid).ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::process::{Command, Stdio};

    fn report(lease_id: &str, pid: u32) -> DaemonHealthReport {
        DaemonHealthReport {
            freshness: DaemonFreshnessReport {
                fresh: false,
                stale_reason_code: Some(DaemonStaleReasonCode::VersionMismatch),
                restartable: true,
                lease_id: Some(lease_id.to_string()),
                pid: Some(pid),
                recovery_evidence: None,
                ownership_evidence: None,
                adoption_command: None,
                binary_hash: None,
                daemon_version: Some("0.1.0".to_string()),
                daemon_build_identity: Some("homeboy 0.1.0+stale".to_string()),
                runtime_paths: None,
                active_jobs: 1,
                termination_evidence: None,
                repair_plan: Vec::new(),
            },
            pid: Some(pid),
            build_identity: None,
        }
    }

    fn daemon() -> RemoteDaemon {
        RemoteDaemon {
            address: "127.0.0.1:7331".to_string(),
            pid: Some(7331),
            lease_id: Some("lease-live".to_string()),
            version: None,
            build_identity: None,
            inspected_freshness: None,
        }
    }

    #[test]
    fn freshness_allows_an_unspecified_version_with_matching_identity_and_hash() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
        let address = listener.local_addr().expect("address");
        let expected_hash = "a".repeat(64);
        let mut freshness = report("lease-candidate", 4242).freshness;
        freshness.fresh = true;
        freshness.binary_hash = Some(expected_hash.clone());
        let body = serde_json::json!({
            "version": "test",
            "build_identity": { "display": "homeboy test+candidate" },
            "lease": { "lease_id": "lease-candidate" },
            "freshness": freshness,
        })
        .to_string();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("health request");
            let mut request = [0; 1024];
            let read = stream.read(&mut request).expect("read request");
            assert!(std::str::from_utf8(&request[..read])
                .expect("request text")
                .starts_with("GET /health HTTP/1.1"));
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(), body
                    )
                    .as_bytes(),
                )
                .expect("health response");
        });

        let report = daemon_http_freshness(
            "homeboy-lab",
            &format!("http://{address}"),
            "",
            "homeboy test+candidate",
        )
        .expect("freshness report");
        assert!(report.fresh);
        assert_eq!(report.binary_hash.as_deref(), Some(expected_hash.as_str()));
        server.join().expect("server");
    }

    #[test]
    fn freshness_rejects_a_nonempty_wrong_expected_version() {
        let body = serde_json::json!({
            "version": "test",
            "build_identity": { "display": "homeboy test+candidate" },
            "lease": { "lease_id": "lease-candidate" },
        });

        let mismatch = daemon_version_identity_mismatch(
            &body,
            "fixture body",
            "wrong-version",
            "homeboy test+candidate",
        )
        .expect("version mismatch result");

        assert_eq!(
            mismatch.as_deref(),
            Some("version test != configured runner version wrong-version")
        );
    }

    #[test]
    fn tunnel_health_rejects_lease_mismatch() {
        assert!(!health_identity_matches(
            &report("lease-other", 7331),
            &daemon()
        ));
    }

    #[test]
    fn tunnel_health_rejects_pid_mismatch() {
        assert!(!health_identity_matches(
            &report("lease-live", 7332),
            &daemon()
        ));
    }

    #[test]
    fn loopback_liveness_requires_the_recorded_daemon_identity() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
        let address = listener.local_addr().expect("address");
        let body = serde_json::json!({
            "freshness": report("lease-live", 7331).freshness,
            "pid": 7331,
        })
        .to_string();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("health request");
            let mut request = [0; 1024];
            let read = stream.read(&mut request).expect("read request");
            assert!(std::str::from_utf8(&request[..read])
                .expect("request text")
                .starts_with("GET /health HTTP/1.1"));
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(), body
                    )
                    .as_bytes(),
                )
                .expect("health response");
        });

        let endpoint = format!("http://{address}");
        assert!(daemon_http_health_matches_with_timeout(
            &endpoint,
            Some("lease-live"),
            Some(7331),
            Duration::from_secs(2),
        ));
        server.join().expect("server");
    }

    #[test]
    fn loopback_liveness_preserves_legacy_pid_only_sessions() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
        let address = listener.local_addr().expect("address");
        let body = serde_json::json!({
            "freshness": report("lease-live", 7331).freshness,
            "pid": 7331,
        })
        .to_string();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("health request");
            let mut request = [0; 1024];
            let _ = stream.read(&mut request).expect("read request");
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(), body
                    )
                    .as_bytes(),
                )
                .expect("health response");
        });

        assert!(daemon_http_health_matches_with_timeout(
            &format!("http://{address}"),
            None,
            Some(7331),
            Duration::from_secs(2),
        ));
        server.join().expect("server");
    }

    #[test]
    fn tunnel_health_accepts_legacy_response_without_pid() {
        let mut report = report("lease-live", 7331);
        report.pid = None;
        assert!(health_identity_matches(&report, &daemon()));
    }

    #[test]
    fn health_pid_is_read_from_the_daemon_health_body() {
        let body = serde_json::json!({ "pid": 7331 });
        assert_eq!(daemon_pid_from_body(&body), Some(7331));
    }

    #[test]
    fn controller_frame_preserves_dead_lease_adoption_eligibility() {
        let mut conflict = report("lease-dead", 7331).freshness;
        conflict.stale_reason_code = Some(DaemonStaleReasonCode::PidDead);
        conflict.recovery_evidence = Some(homeboy_core::daemon::DaemonRecoveryEvidence::ProvenDead);
        conflict.active_jobs = 0;
        conflict.adoption_command = None;
        conflict.repair_plan.clear();

        let conflict = into_controller_frame(conflict, "remote-lab");
        assert!(conflict.adoption_command.is_none());
        assert!(conflict
            .repair_plan
            .iter()
            .all(|step| step.code != daemon_repair::RUNNER_ADOPT_ORPHAN_LEASE));

        let mut eligible = report("lease-dead", 7331).freshness;
        eligible.stale_reason_code = Some(DaemonStaleReasonCode::PidDead);
        eligible.recovery_evidence = Some(homeboy_core::daemon::DaemonRecoveryEvidence::ProvenDead);
        eligible.active_jobs = 0;
        eligible.adoption_command = Some(
            "homeboy daemon adopt-orphan --lease-id lease-dead --confirm-pid-dead".to_string(),
        );

        let eligible = into_controller_frame(eligible, "remote-lab");
        assert_eq!(
            eligible.adoption_command.as_deref(),
            Some("homeboy runner connect remote-lab --adopt-orphan-lease lease-dead --confirm-pid-dead")
        );
        assert_eq!(eligible.repair_plan.len(), 1);
    }

    /// #8459: a freshly started daemon can accept the TCP connection before its
    /// HTTP handler answers `/health`, so the first probe fails with a
    /// transport error. The probe must retry within its settle budget and
    /// converge instead of failing the reconnect (which triggered a full
    /// refresh rollback and manual orphan-lease recovery).
    #[test]
    fn health_probe_retries_through_a_transient_startup_failure() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
        let address = listener.local_addr().expect("address");
        let body = serde_json::json!({
            "freshness": report("lease-live", 7331).freshness,
            "pid": 7331,
        })
        .to_string();
        let server = std::thread::spawn(move || {
            // First connection: accept then drop without responding, forcing an
            // `error sending request` transport failure on the controller side.
            let (first, _) = listener.accept().expect("first health request");
            drop(first);
            // The retry and both post-establishment observations must answer.
            for _ in 0..3 {
                let (mut stream, _) = listener.accept().expect("health request");
                let mut request = [0; 1024];
                let _ = stream.read(&mut request).expect("read request");
                stream
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            body.len(), body
                        )
                        .as_bytes(),
                    )
                    .expect("health response");
            }
        });

        let daemon = RemoteDaemon {
            address: address.to_string(),
            pid: Some(7331),
            lease_id: Some("lease-live".to_string()),
            version: None,
            build_identity: None,
            inspected_freshness: None,
        };
        let started = std::time::Instant::now();
        let result =
            probe_daemon_health_until_durable(&format!("http://{address}"), &daemon, None, None);
        assert!(
            result.is_ok(),
            "probe must retry through startup and observe durable health: {result:?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "bounded health settle and durability observations must not hang"
        );
        server.join().expect("server");
    }

    #[test]
    fn unreachable_health_probe_is_bounded() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
        let address = listener.local_addr().expect("address");
        drop(listener);
        let daemon = RemoteDaemon {
            address: address.to_string(),
            pid: Some(7331),
            lease_id: Some("lease-live".to_string()),
            version: None,
            build_identity: None,
            inspected_freshness: None,
        };

        let started = std::time::Instant::now();
        let result =
            probe_daemon_health_until_durable(&format!("http://{address}"), &daemon, None, None);
        assert!(matches!(
            result,
            Err(DaemonHealthProbeFailure::Unreachable { attempts, .. }) if attempts.len() == 4
        ));
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "four bounded refused health probes must not hang"
        );
    }

    /// An identity mismatch is authoritative — the daemon is up but is the
    /// wrong one — so the probe must fail immediately without burning the
    /// settle budget on retries.
    #[test]
    fn health_probe_does_not_retry_an_identity_mismatch() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
        let address = listener.local_addr().expect("address");
        let body = serde_json::json!({
            "freshness": report("lease-other", 7331).freshness,
            "pid": 7331,
        })
        .to_string();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("health request");
            let mut request = [0; 1024];
            let _ = stream.read(&mut request).expect("read request");
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(), body
                    )
                    .as_bytes(),
                )
                .expect("health response");
        });

        let daemon = RemoteDaemon {
            address: address.to_string(),
            pid: Some(7331),
            lease_id: Some("lease-live".to_string()),
            version: None,
            build_identity: None,
            inspected_freshness: None,
        };
        let started = std::time::Instant::now();
        let result =
            probe_daemon_health_until_durable(&format!("http://{address}"), &daemon, None, None);
        assert!(
            matches!(result, Err(DaemonHealthProbeFailure::IdentityMismatch(_))),
            "identity mismatch must fail immediately: {result:?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "identity mismatch must not burn the retry budget"
        );
        server.join().expect("server");
    }

    #[test]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn durability_check_rejects_a_tunnel_that_exits_after_apparent_readiness() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
        let address = listener.local_addr().expect("address");
        let body = serde_json::json!({
            "freshness": report("lease-live", 7331).freshness,
            "pid": 7331,
        })
        .to_string();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("initial health request");
            let mut request = [0; 1024];
            let _ = stream.read(&mut request).expect("read request");
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(), body
                    )
                    .as_bytes(),
                )
                .expect("health response");
        });
        let mut child = Command::new("sh")
            .arg("-c")
            .arg("sleep 0.05")
            .spawn()
            .expect("start tunnel child");
        let pid = child.id();
        let identity = super::super::capture_tunnel_process_start_identity(Some(pid))
            .expect("capture child identity")
            .expect("child identity");
        let daemon = RemoteDaemon {
            address: address.to_string(),
            pid: Some(7331),
            lease_id: Some("lease-live".to_string()),
            version: None,
            build_identity: None,
            inspected_freshness: None,
        };

        let started = std::time::Instant::now();
        let result = probe_daemon_health_until_durable(
            &format!("http://{address}"),
            &daemon,
            Some(pid),
            Some(&identity),
        );
        let Err(failure) = result else {
            panic!("exited tunnel must fail the durability window");
        };
        assert!(
            matches!(failure, DaemonHealthProbeFailure::TunnelExited { .. }),
            "{failure:?}"
        );
        assert_eq!(failure_stage(&failure), "tunnel_durability");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "an exited tunnel must fail within the bounded durability window"
        );
        assert!(child.wait().expect("reap tunnel child").success());
        server.join().expect("server");
    }

    #[test]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn durability_health_failure_rechecks_tunnel_identity() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
        let address = listener.local_addr().expect("address");
        let body = serde_json::json!({
            "freshness": report("lease-live", 7331).freshness,
            "pid": 7331,
        })
        .to_string();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("initial health request");
            let mut request = [0; 1024];
            let _ = stream.read(&mut request).expect("read request");
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(), body
                    )
                    .as_bytes(),
                )
                .expect("health response");
            let (second, _) = listener.accept().expect("durability health request");
            std::thread::sleep(Duration::from_millis(100));
            drop(second);
        });
        let mut child = Command::new("sh")
            .arg("-c")
            .arg("sleep 0.3")
            .spawn()
            .expect("start tunnel child");
        let pid = child.id();
        let identity = super::super::capture_tunnel_process_start_identity(Some(pid))
            .expect("capture child identity")
            .expect("child identity");
        let daemon = RemoteDaemon {
            address: address.to_string(),
            pid: Some(7331),
            lease_id: Some("lease-live".to_string()),
            version: None,
            build_identity: None,
            inspected_freshness: None,
        };

        let failure = probe_daemon_health_until_durable(
            &format!("http://{address}"),
            &daemon,
            Some(pid),
            Some(&identity),
        )
        .expect_err("exited tunnel after durability health failure is terminal");
        assert!(
            matches!(failure, DaemonHealthProbeFailure::TunnelExited { .. }),
            "{failure:?}"
        );
        assert_eq!(failure_stage(&failure), "tunnel_durability");
        assert!(child.wait().expect("reap tunnel child").success());
        server.join().expect("server");
    }

    #[test]
    fn loopback_durability_has_no_ssh_identity_requirement() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
        let address = listener.local_addr().expect("address");
        let body = serde_json::json!({
            "freshness": report("lease-live", 7331).freshness,
            "pid": 7331,
        })
        .to_string();
        let server = std::thread::spawn(move || {
            for _ in 0..3 {
                let (mut stream, _) = listener.accept().expect("health request");
                let mut request = [0; 1024];
                let _ = stream.read(&mut request).expect("read request");
                stream
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            body.len(), body
                        )
                        .as_bytes(),
                    )
                    .expect("health response");
            }
        });
        let daemon = RemoteDaemon {
            address: address.to_string(),
            pid: Some(7331),
            lease_id: Some("lease-live".to_string()),
            version: None,
            build_identity: None,
            inspected_freshness: None,
        };

        let started = std::time::Instant::now();
        assert!(probe_daemon_health_until_durable(
            &format!("http://{address}"),
            &daemon,
            None,
            None,
        )
        .is_ok());
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "loopback durability observations must not hang"
        );
        server.join().expect("server");
    }
}
