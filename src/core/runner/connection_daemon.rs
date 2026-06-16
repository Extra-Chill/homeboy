use std::path::PathBuf;
use std::time::Duration;

use reqwest::blocking::Client;
use serde_json::Value;

use crate::core::engine::shell;
use crate::core::server::{Server, SshClient};

use super::{
    command_failure_message, failed_connect, open_loopback_tunnel, parse_loopback_daemon_addr,
    remote_daemon_start, reserve_loopback_port, terminate_pid, wait_for_tcp, RemoteDaemon,
};
use crate::core::runner::{RunnerConnectReport, RunnerFailureKind};

pub(super) fn connect_remote_daemon(
    server: &Server,
    client: &SshClient,
    homeboy: &str,
    daemon: RemoteDaemon,
    expected_version: &str,
    expected_identity: &str,
    runner_id: &str,
    session_path: &PathBuf,
) -> std::result::Result<(u16, Option<u32>, String, RemoteDaemon), (RunnerConnectReport, i32)> {
    let failed_after_tunnel = |tunnel_pid: Option<u32>, message: String| {
        if let Some(pid) = tunnel_pid {
            terminate_pid(pid);
        }
        failed_connect(
            runner_id,
            session_path.clone(),
            RunnerFailureKind::DaemonStartupFailure,
            message,
        )
    };
    let (local_port, tunnel_pid, local_url) =
        open_daemon_tunnel(server, &daemon, runner_id, session_path)?;
    match daemon_freshness_mismatch(&local_url, expected_version, expected_identity) {
        Ok(None) => Ok((local_port, tunnel_pid, local_url, daemon)),
        Ok(Some(_)) => {
            if let Some(pid) = tunnel_pid {
                terminate_pid(pid);
            }
            if let Err(message) = remote_daemon_stop(client, homeboy) {
                return Err(failed_connect(
                    runner_id,
                    session_path.clone(),
                    RunnerFailureKind::DaemonStartupFailure,
                    message,
                ));
            }
            let daemon = match remote_daemon_start(client, homeboy) {
                Ok(daemon) => daemon,
                Err(message) => {
                    return Err(failed_connect(
                        runner_id,
                        session_path.clone(),
                        RunnerFailureKind::DaemonStartupFailure,
                        message,
                    ));
                }
            };
            let (local_port, tunnel_pid, local_url) =
                open_daemon_tunnel(server, &daemon, runner_id, session_path)?;
            match daemon_freshness_mismatch(&local_url, expected_version, expected_identity) {
                Ok(None) => {}
                Ok(Some(reason)) => {
                    return Err(failed_after_tunnel(
                        tunnel_pid,
                        format!(
                            "remote daemon restarted but still reports stale Homeboy build: {reason}"
                        ),
                    ));
                }
                Err(message) => {
                    return Err(failed_after_tunnel(tunnel_pid, message));
                }
            }
            Ok((local_port, tunnel_pid, local_url, daemon))
        }
        Err(message) => Err(failed_after_tunnel(tunnel_pid, message)),
    }
}

fn open_daemon_tunnel(
    server: &Server,
    daemon: &RemoteDaemon,
    runner_id: &str,
    session_path: &PathBuf,
) -> std::result::Result<(u16, Option<u32>, String), (RunnerConnectReport, i32)> {
    let Ok(remote_addr) = parse_loopback_daemon_addr(&daemon.address) else {
        return Err(failed_connect(
            runner_id,
            session_path.clone(),
            RunnerFailureKind::DaemonStartupFailure,
            "remote daemon did not report a loopback address".to_string(),
        ));
    };

    let local_port = reserve_loopback_port().map_err(|err| {
        failed_connect(
            runner_id,
            session_path.clone(),
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
            session_path.clone(),
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
            session_path.clone(),
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
    let body = daemon_http_body(local_url)?;
    daemon_identity_from_body(&body)
        .filter(|identity| !identity.trim().is_empty())
        .map(|identity| identity.trim().to_string())
        .ok_or_else(|| {
            "remote daemon version response did not include a build identity".to_string()
        })
}

pub(super) fn daemon_http_version(local_url: &str) -> std::result::Result<String, String> {
    let body = daemon_http_body(local_url)?;
    daemon_version_from_body(&body)
        .filter(|version| !version.trim().is_empty())
        .map(|version| version.trim().to_string())
        .ok_or_else(|| "remote daemon version response did not include a version".to_string())
}

fn daemon_http_body(local_url: &str) -> std::result::Result<Value, String> {
    let client = Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .map_err(|err| format!("build daemon HTTP client: {err}"))?;
    let response = client
        .get(format!("{}/version", local_url.trim_end_matches('/')))
        .send()
        .map_err(|err| format!("query remote daemon version: {err}"))?;
    let status_code = response.status().as_u16();
    let body: Value = response
        .json()
        .map_err(|err| format!("parse remote daemon version response: {err}"))?;
    if status_code >= 400 {
        return Err(format!(
            "remote daemon version request failed with HTTP {}: {}",
            status_code, body
        ));
    }
    Ok(body)
}

pub(super) fn daemon_version_from_body(body: &Value) -> Option<&str> {
    body.get("version").and_then(Value::as_str).or_else(|| {
        body.get("data")
            .and_then(|data| data.get("version"))
            .and_then(Value::as_str)
    })
}

pub(super) fn daemon_identity_from_body(body: &Value) -> Option<&str> {
    body.pointer("/build_identity/display")
        .and_then(Value::as_str)
        .or_else(|| {
            body.pointer("/data/build_identity/display")
                .and_then(Value::as_str)
        })
}

fn daemon_freshness_mismatch(
    local_url: &str,
    expected_version: &str,
    expected_identity: &str,
) -> std::result::Result<Option<String>, String> {
    let running_version = daemon_http_version(local_url)?;
    if !versions_match(&running_version, expected_version) {
        return Ok(Some(format!(
            "version {running_version} != configured runner version {expected_version}"
        )));
    }

    let running_identity = match daemon_http_identity(local_url) {
        Ok(identity) => identity,
        Err(message) => return Ok(Some(message)),
    };
    if !versions_match(&running_identity, expected_identity) {
        return Ok(Some(format!(
            "identity {running_identity} != configured runner identity {expected_identity}"
        )));
    }

    Ok(None)
}

fn remote_daemon_stop(client: &SshClient, homeboy: &str) -> std::result::Result<(), String> {
    let command = format!("{} daemon stop", shell::quote_arg(homeboy));
    let output = client.execute(&command);
    if !output.success {
        return Err(command_failure_message(
            "remote daemon stop failed while refreshing stale daemon",
            &output,
        ));
    }
    Ok(())
}
