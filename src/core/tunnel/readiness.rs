use std::fs;
use std::net::{TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

use crate::core::error::{Error, Result};

use super::runtime::runtime_state_is_running;
use super::types::*;

pub(super) fn wait_until_ready(state: &ServiceTunnelRuntimeState, timeout_secs: u64) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    loop {
        let readiness = check_runtime_readiness(state);
        if !readiness.process_running {
            return Err(Error::validation_invalid_argument(
                "service",
                "service process exited before becoming ready",
                Some(state.preview_identity.service_id.clone()),
                None,
            ));
        }
        if readiness.ready {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(Error::validation_invalid_argument(
                "readiness",
                "service did not satisfy readiness before timeout",
                Some(state.preview_identity.service_id.clone()),
                Some(
                    readiness
                        .checks
                        .into_iter()
                        .filter(|check| !check.ready)
                        .filter_map(|check| check.detail)
                        .collect(),
                ),
            ));
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

pub(super) fn check_runtime_readiness(
    state: &ServiceTunnelRuntimeState,
) -> ServiceTunnelReadinessStatus {
    let process_running = runtime_state_is_running(state);
    let mut checks = Vec::new();

    if state.health_url.is_some() {
        let health = check_runtime_health(state);
        checks.push(ServiceTunnelReadinessCheckStatus {
            check: "health".to_string(),
            ready: health.healthy,
            detail: health
                .status_code
                .map(|status| format!("status {status}"))
                .or(health.error),
        });
    }

    for check in &state.readiness_checks {
        checks.push(evaluate_readiness_check(state, check));
    }

    let checks_ready = checks.iter().all(|check| check.ready);
    let ready = process_running && checks_ready;
    ServiceTunnelReadinessStatus {
        kind: state.readiness_kind.clone(),
        process_running,
        ready,
        preview_ready: matches!(state.readiness_kind, ServiceTunnelReadinessKind::Preview) && ready,
        proof_ready: matches!(state.readiness_kind, ServiceTunnelReadinessKind::Proof) && ready,
        checks,
    }
}

fn evaluate_readiness_check(
    state: &ServiceTunnelRuntimeState,
    check: &ServiceTunnelReadinessCheck,
) -> ServiceTunnelReadinessCheckStatus {
    match check {
        ServiceTunnelReadinessCheck::TcpListener => tcp_listener_readiness(state),
        ServiceTunnelReadinessCheck::ArtifactJsonPointer {
            path,
            pointer,
            equals,
        } => artifact_json_pointer_readiness(path, pointer, equals),
        ServiceTunnelReadinessCheck::StdoutRegex { pattern } => {
            stdout_regex_readiness(&state.logs.stdout_path, pattern)
        }
    }
}

fn tcp_listener_readiness(state: &ServiceTunnelRuntimeState) -> ServiceTunnelReadinessCheckStatus {
    let Some((host, port)) = local_url_host_port(&state.local_url) else {
        return ServiceTunnelReadinessCheckStatus {
            check: "tcp_listener".to_string(),
            ready: false,
            detail: Some(format!("could not parse local URL {}", state.local_url)),
        };
    };
    let address = format!("{host}:{port}");
    match address.to_socket_addrs() {
        Ok(mut addresses) => {
            let ready = addresses.any(|address| {
                TcpStream::connect_timeout(&address, Duration::from_millis(200)).is_ok()
            });
            ServiceTunnelReadinessCheckStatus {
                check: "tcp_listener".to_string(),
                ready,
                detail: Some(address),
            }
        }
        Err(error) => ServiceTunnelReadinessCheckStatus {
            check: "tcp_listener".to_string(),
            ready: false,
            detail: Some(error.to_string()),
        },
    }
}

fn artifact_json_pointer_readiness(
    path: &str,
    pointer: &str,
    equals: &str,
) -> ServiceTunnelReadinessCheckStatus {
    let data = match fs::read_to_string(path) {
        Ok(data) => data,
        Err(error) => {
            return ServiceTunnelReadinessCheckStatus {
                check: "artifact_json_pointer".to_string(),
                ready: false,
                detail: Some(format!("{}: {error}", path)),
            };
        }
    };
    let json: serde_json::Value = match serde_json::from_str(&data) {
        Ok(json) => json,
        Err(error) => {
            return ServiceTunnelReadinessCheckStatus {
                check: "artifact_json_pointer".to_string(),
                ready: false,
                detail: Some(format!("{}: {error}", path)),
            };
        }
    };
    let Some(value) = json.pointer(pointer) else {
        return ServiceTunnelReadinessCheckStatus {
            check: "artifact_json_pointer".to_string(),
            ready: false,
            detail: Some(format!("{path} missing {pointer}")),
        };
    };
    let actual = value
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| value.to_string());
    ServiceTunnelReadinessCheckStatus {
        check: "artifact_json_pointer".to_string(),
        ready: actual == equals,
        detail: Some(format!("{path} {pointer}={actual}")),
    }
}

fn stdout_regex_readiness(path: &str, pattern: &str) -> ServiceTunnelReadinessCheckStatus {
    let data = match fs::read_to_string(path) {
        Ok(data) => data,
        Err(error) => {
            return ServiceTunnelReadinessCheckStatus {
                check: "stdout_regex".to_string(),
                ready: false,
                detail: Some(format!("{}: {error}", path)),
            };
        }
    };
    match regex::Regex::new(pattern) {
        Ok(regex) => ServiceTunnelReadinessCheckStatus {
            check: "stdout_regex".to_string(),
            ready: regex.is_match(&data),
            detail: Some(pattern.to_string()),
        },
        Err(error) => ServiceTunnelReadinessCheckStatus {
            check: "stdout_regex".to_string(),
            ready: false,
            detail: Some(error.to_string()),
        },
    }
}

fn local_url_host_port(local_url: &str) -> Option<(String, u16)> {
    let url = reqwest::Url::parse(local_url).ok()?;
    Some((url.host_str()?.to_string(), url.port_or_known_default()?))
}

pub(super) fn check_runtime_health(state: &ServiceTunnelRuntimeState) -> ServiceTunnelHealthStatus {
    let Some(url) = state.health_url.clone() else {
        return ServiceTunnelHealthStatus {
            checked: false,
            healthy: runtime_state_is_running(state),
            url: None,
            status_code: None,
            error: None,
        };
    };

    match reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .and_then(|client| client.get(&url).send())
    {
        Ok(response) => ServiceTunnelHealthStatus {
            checked: true,
            healthy: response.status().is_success(),
            url: Some(url),
            status_code: Some(response.status().as_u16()),
            error: None,
        },
        Err(error) => ServiceTunnelHealthStatus {
            checked: true,
            healthy: false,
            url: Some(url),
            status_code: None,
            error: Some(error.to_string()),
        },
    }
}
