use std::path::Path;
use std::time::Duration;

use reqwest::blocking::Client;
use serde::Deserialize;
use serde_json::Value;

use crate::core::daemon::{DaemonFreshnessReport, DaemonStaleReasonCode};
use crate::core::server::{Server, SshClient};

use super::super::session::RunnerStaleRuntimePath;
use super::{
    failed_connect, open_loopback_tunnel, parse_loopback_daemon_addr, remote_daemon_start,
    remote_daemon_stop, reserve_loopback_port, terminate_pid, wait_for_tcp, RemoteDaemon,
};
use crate::core::runner::connection::remote_daemon::parse_json_from_mixed_stdout;
use crate::core::runner::daemon_freshness::repair_or_fail;
use crate::core::runner::{RunnerConnectReport, RunnerFailureKind};
use std::collections::BTreeMap;

#[derive(Debug)]
struct DaemonVersionResponse {
    body: Value,
    raw_body: String,
}

pub(super) fn connect_remote_daemon(
    server: &Server,
    client: &SshClient,
    homeboy: &str,
    daemon: RemoteDaemon,
    expected_version: &str,
    expected_identity: &str,
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
    match daemon_freshness_report(&local_url, expected_version, expected_identity) {
        Ok(report) if report.fresh => Ok((local_port, tunnel_pid, local_url, daemon)),
        Ok(report) if repair_or_fail(&report).is_ok() => {
            if let Some(pid) = tunnel_pid {
                terminate_pid(pid);
            }
            if let Err(message) = remote_daemon_stop(client, homeboy) {
                return Err(failed_connect(
                    runner_id,
                    session_path.to_path_buf(),
                    RunnerFailureKind::DaemonStartupFailure,
                    message,
                ));
            }
            let daemon = match remote_daemon_start(client, homeboy) {
                Ok(daemon) => daemon,
                Err(message) => {
                    return Err(failed_connect(
                        runner_id,
                        session_path.to_path_buf(),
                        RunnerFailureKind::DaemonStartupFailure,
                        message,
                    ));
                }
            };
            let (local_port, tunnel_pid, local_url) =
                open_daemon_tunnel(server, &daemon, runner_id, session_path)?;
            match daemon_freshness_report(&local_url, expected_version, expected_identity) {
                Ok(report) if report.fresh => {}
                Ok(report) => {
                    return Err(failed_after_tunnel(
                        tunnel_pid,
                        format!(
                            "remote daemon restarted but still reports stale Homeboy build: {:?}",
                            report.stale_reason_code
                        ),
                    ));
                }
                Err(message) => {
                    return Err(failed_after_tunnel(tunnel_pid, message));
                }
            }
            Ok((local_port, tunnel_pid, local_url, daemon))
        }
        Ok(report) => Err(failed_after_tunnel(
            tunnel_pid,
            repair_or_fail(&report).unwrap_err(),
        )),
        Err(message) => Err(failed_after_tunnel(tunnel_pid, message)),
    }
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

fn daemon_http_body(local_url: &str) -> std::result::Result<DaemonVersionResponse, String> {
    let client = Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .map_err(|err| format!("build daemon HTTP client: {err}"))?;
    let response = client
        .get(format!("{}/version", local_url.trim_end_matches('/')))
        .send()
        .map_err(|err| format!("query remote daemon version: {err}"))?;
    let status_code = response.status().as_u16();
    let body_text = response
        .text()
        .map_err(|err| format!("read remote daemon version response: {err}"))?;
    let body: Value = parse_json_from_mixed_stdout(&body_text).map_err(|err| {
        format!(
            "parse remote daemon version response: {err}; raw body: {}",
            response_body_excerpt(&body_text)
        )
    })?;
    if status_code >= 400 {
        return Err(format!(
            "remote daemon version request failed with HTTP {}: {}",
            status_code, body
        ));
    }
    Ok(DaemonVersionResponse {
        body,
        raw_body: body_text,
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
        binary_hash: None,
        runtime_paths: None,
        active_jobs: 0,
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
