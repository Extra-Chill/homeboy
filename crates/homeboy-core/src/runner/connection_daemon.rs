use std::path::Path;
use std::time::Duration;

use reqwest::blocking::Client;
use serde::Deserialize;
use serde_json::Value;

use crate::daemon::{DaemonFreshnessReport, DaemonStaleReasonCode};
use crate::server::{Server, SshClient};

use super::super::session::RunnerStaleRuntimePath;
use super::{
    failed_connect, open_loopback_tunnel, parse_loopback_daemon_addr, reserve_loopback_port,
    terminate_pid, wait_for_tcp, RemoteDaemon,
};
use crate::runner::connection::remote_daemon::parse_json_from_mixed_stdout;
use crate::runner::{RunnerConnectReport, RunnerFailureKind};
use std::collections::BTreeMap;

#[derive(Debug)]
struct DaemonVersionResponse {
    body: Value,
    raw_body: String,
}

#[derive(Debug)]
struct DaemonHealthReport {
    freshness: DaemonFreshnessReport,
    pid: Option<u32>,
}

pub(super) fn connect_remote_daemon(
    server: &Server,
    _client: &SshClient,
    _homeboy: &str,
    daemon: RemoteDaemon,
    _expected_version: &str,
    _expected_identity: &str,
    runner_id: &str,
    session_path: &Path,
) -> std::result::Result<(u16, Option<u32>, String, RemoteDaemon), (RunnerConnectReport, i32)> {
    let failed_after_tunnel = |tunnel_pid: Option<u32>, message: String| {
        if let Some(pid) = tunnel_pid {
            terminate_pid(pid);
        }
        failed_connect(
            runner_id,
            session_path.to_path_buf(),
            RunnerFailureKind::DaemonStartupFailure,
            message,
        )
    };
    let (local_port, tunnel_pid, local_url) =
        open_daemon_tunnel(server, &daemon, runner_id, session_path)?;
    match daemon_health_report(&local_url) {
        Ok(report) if health_identity_matches(&report, &daemon) => {
            Ok((local_port, tunnel_pid, local_url, daemon))
        }
        Ok(report) => Err(failed_after_tunnel(
            tunnel_pid,
            format!(
                "remote daemon health identity changed or is unavailable (expected lease {:?}, PID {:?}; got lease {:?}, PID {:?}); refusing to write session{}",
                daemon.lease_id, daemon.pid, report.freshness.lease_id, report.pid,
                active_job_recovery_guidance(&daemon),
            ),
        )),
        Err(message) => Err(failed_after_tunnel(tunnel_pid, message)),
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
) -> std::result::Result<(u16, Option<u32>, String), (RunnerConnectReport, i32)> {
    let Ok(remote_addr) = parse_loopback_daemon_addr(&daemon.address) else {
        return Err(failed_connect(
            runner_id,
            session_path.to_path_buf(),
            RunnerFailureKind::DaemonStartupFailure,
            "remote daemon did not report a loopback address".to_string(),
        ));
    };

    let local_port = reserve_loopback_port().map_err(|err| {
        failed_connect(
            runner_id,
            session_path.to_path_buf(),
            RunnerFailureKind::TunnelFailure,
            err.to_string(),
        )
    })?;
    let tunnel = open_loopback_tunnel(
        server,
        local_port,
        &remote_addr.ip().to_string(),
        remote_addr.port(),
    );
    if !tunnel.success {
        return Err(failed_connect(
            runner_id,
            session_path.to_path_buf(),
            RunnerFailureKind::TunnelFailure,
            format!("SSH tunnel setup failed: {}", tunnel.stderr.trim()),
        ));
    }

    if !wait_for_tcp(local_port, Duration::from_secs(5)) {
        if let Some(pid) = tunnel.pid {
            terminate_pid(pid);
        }
        return Err(failed_connect(
            runner_id,
            session_path.to_path_buf(),
            RunnerFailureKind::TunnelFailure,
            format!(
                "local tunnel 127.0.0.1:{} did not become reachable",
                local_port
            ),
        ));
    }
    Ok((
        local_port,
        tunnel.pid,
        format!("http://127.0.0.1:{}", local_port),
    ))
}

pub(super) fn versions_match(left: &str, right: &str) -> bool {
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

pub(super) fn daemon_http_runtime_stale_paths(
    local_url: &str,
) -> std::result::Result<Vec<RunnerStaleRuntimePath>, String> {
    let response = daemon_http_body(local_url)?;
    Ok(daemon_runtime_stale_paths_from_body(&response.body))
}

pub(super) fn daemon_http_runtime_loaded_paths(
    local_url: &str,
) -> std::result::Result<BTreeMap<String, String>, String> {
    let response = daemon_http_body(local_url)?;
    Ok(daemon_runtime_loaded_paths_from_body(&response.body))
}

pub(super) fn daemon_http_freshness(
    local_url: &str,
    expected_version: &str,
    expected_identity: &str,
) -> std::result::Result<DaemonFreshnessReport, String> {
    daemon_freshness_report(local_url, expected_version, expected_identity)
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

fn daemon_health_report_with_timeout(
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

fn daemon_freshness_report(
    local_url: &str,
    expected_version: &str,
    expected_identity: &str,
) -> std::result::Result<DaemonFreshnessReport, String> {
    let DaemonVersionResponse { body, raw_body } = daemon_http_body(local_url)?;
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
            return Ok(report);
        }
        if report.fresh {
            let mut report = report;
            report.fresh = false;
            report.stale_reason_code = Some(DaemonStaleReasonCode::VersionMismatch);
            report.restartable = true;
            return Ok(report);
        }
        return Ok(report);
    }
    let mismatch =
        daemon_version_identity_mismatch(&body, &raw_body, expected_version, expected_identity)?;
    Ok(DaemonFreshnessReport {
        fresh: mismatch.is_none(),
        stale_reason_code: mismatch.map(|_| DaemonStaleReasonCode::VersionMismatch),
        restartable: true,
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
    })
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
    if !versions_match(&running_version, expected_version) {
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
}
