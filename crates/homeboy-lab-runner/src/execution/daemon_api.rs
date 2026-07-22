use std::time::Duration;

use reqwest::blocking::Client;
use reqwest::blocking::RequestBuilder;
use reqwest::header::CONNECTION;
use serde_json::{json, Value};

use homeboy_core::error::{Error, ErrorCode, Result};

use super::super::broker_http;
use super::super::daemon_http_get::{daemon_get, parse_daemon_response_json};
use super::super::{load, status, RunnerSession, RunnerTunnelMode};

#[allow(unused_imports)]
use super::*;

fn unsupported_daemon_api_method(method: &str) -> Error {
    Error::internal_unexpected(format!("unsupported daemon API method {method}"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DaemonHttpErrorKind {
    Connect,
    Timeout,
    Status,
    BodyDecode,
}

impl DaemonHttpErrorKind {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            DaemonHttpErrorKind::Connect => "connect",
            DaemonHttpErrorKind::Timeout => "timeout",
            DaemonHttpErrorKind::Status => "status",
            DaemonHttpErrorKind::BodyDecode => "body_decode",
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub(super) struct DaemonPostOptions {
    pub(super) connection_close: bool,
}

pub(super) struct DaemonHttpTextResponse {
    pub(super) status_code: u16,
    pub(super) body: String,
}

pub fn canonical_daemon_body<'a>(data: &'a Value, context: &str) -> Result<&'a Value> {
    data.get("body")
        .ok_or_else(|| Error::internal_unexpected(format!("{context} missing canonical data.body")))
}

fn reverse_broker_daemon_data(body: Value) -> Value {
    json!({ "body": body })
}

pub(super) fn daemon_transport_error(
    kind: DaemonHttpErrorKind,
    path: &str,
    status_code: Option<u16>,
    context: &str,
    error: impl Into<String>,
) -> Error {
    let mut err = Error::new(
        ErrorCode::InternalUnexpected,
        format!("{context}: {}", error.into()),
        json!({
            "daemon_transport_error": {
                "kind": kind.as_str(),
                "path": path,
                "http_status": status_code,
            }
        }),
    );
    err.retryable = Some(true);
    err
}

fn classify_reqwest_error(err: &reqwest::Error) -> DaemonHttpErrorKind {
    if err.is_timeout() {
        DaemonHttpErrorKind::Timeout
    } else if err.is_connect() {
        DaemonHttpErrorKind::Connect
    } else {
        DaemonHttpErrorKind::Status
    }
}

fn with_daemon_post_options(request: RequestBuilder, options: DaemonPostOptions) -> RequestBuilder {
    if options.connection_close {
        request.header(CONNECTION, "close")
    } else {
        request
    }
}

pub fn daemon_api_get(runner_id: &str, path: &str) -> Result<Value> {
    daemon_api_request(runner_id, path, "GET")
}

/// Query one known direct-daemon generation without re-resolving ownership.
/// Generation reconciliation uses this only after a job lookup returned 404.
pub(crate) fn daemon_api_get_for_session(session: &RunnerSession, path: &str) -> Result<Value> {
    let local_url = session.local_url.as_deref().ok_or_else(|| {
        Error::internal_unexpected("known daemon generation has no direct local endpoint")
    })?;
    let client = Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|err| Error::internal_unexpected(format!("build daemon HTTP client: {err}")))?;
    daemon_get(&client, local_url, path)
}

pub fn daemon_api_post(runner_id: &str, path: &str) -> Result<Value> {
    daemon_api_request(runner_id, path, "POST")
}

pub(super) fn daemon_api_request(runner_id: &str, path: &str, method: &str) -> Result<Value> {
    let runner = load(runner_id)?;
    let connected = status(runner_id)?;
    let Some(legacy_session) = connected.session.filter(|_| connected.connected) else {
        return Err(Error::validation_invalid_argument(
            "runner",
            "runner is not connected to a daemon; run `homeboy runner connect <runner-id>` first",
            Some(runner.id),
            Some(vec![
                "Read/query integrations use the connected daemon so results come from the runner machine.".to_string(),
            ]),
        ));
    };
    let session = daemon_api_session_for_path(runner_id, path, legacy_session)?;
    let client = Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|err| Error::internal_unexpected(format!("build daemon HTTP client: {err}")))?;
    if let Some(local_url) = session.local_url.as_deref() {
        return match method {
            "GET" => daemon_get(&client, local_url, path),
            "POST" => daemon_post(&client, local_url, path),
            _ => Err(unsupported_daemon_api_method(method)),
        };
    }
    if session.mode == RunnerTunnelMode::Reverse {
        let Some(broker_url) = session.broker_url.as_deref() else {
            return Err(Error::validation_invalid_argument(
                "runner",
                "reverse runner session does not expose a broker URL",
                Some(runner.id),
                Some(vec![
                    "Reconnect the reverse runner with `homeboy runner connect <controller-id> --reverse --reverse-runner <runner-id> --broker-url <url>`.".to_string(),
                ]),
            ));
        };
        let broker_token = homeboy_core::broker_auth::broker_submit_token_for_runner(runner_id)?;
        let body = match method {
            "GET" => broker_http::get_json(
                &client,
                broker_url,
                path,
                "query reverse runner broker",
                broker_token.as_deref(),
            ),
            "POST" => broker_http::post_json(
                &client,
                broker_url,
                path,
                json!({}),
                "query reverse runner broker",
                broker_token.as_deref(),
            ),
            _ => Err(unsupported_daemon_api_method(method)),
        }?;
        // Broker helpers validate and extract their canonical `data.body`, while
        // daemon API consumers intentionally parse the daemon `data` object.
        // Restore that shared shape so direct and reverse transports are
        // interchangeable for status, logs, artifacts, and cancellation.
        return Ok(reverse_broker_daemon_data(body));
    }
    Err(Error::validation_invalid_argument(
        "runner",
        "runner session does not expose a local daemon URL or reverse broker URL",
        Some(runner.id),
        Some(vec![
            "Use a direct daemon connection or a reverse runner session registered with a broker before querying runner jobs.".to_string(),
        ]),
    ))
}

fn daemon_api_session_for_path(
    runner_id: &str,
    path: &str,
    legacy_session: RunnerSession,
) -> Result<RunnerSession> {
    let segments = path.trim_start_matches('/').split('/').collect::<Vec<_>>();
    let (job_id, run_id, artifact_id) = match segments.as_slice() {
        ["jobs", job_id, "artifacts", artifact_id, ..] => (Some(*job_id), None, Some(*artifact_id)),
        ["jobs", job_id, ..] => (Some(*job_id), None, None),
        ["runs", run_id, ..] => (None, Some(*run_id), None),
        _ => (None, None, None),
    };
    Ok(super::super::generation_store::endpoint_session(
        runner_id,
        job_id.filter(|id| !id.is_empty()),
        run_id.filter(|id| !id.is_empty()),
        artifact_id.filter(|id| !id.is_empty()),
        Some(&legacy_session),
    )?
    .unwrap_or(legacy_session))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RunnerSession, RunnerSessionRole, RunnerTunnelMode};
    use homeboy_core::test_support;

    fn session(lease: &str, endpoint: &str) -> RunnerSession {
        RunnerSession {
            runner_id: "runner-a".to_string(),
            mode: RunnerTunnelMode::DirectSsh,
            role: RunnerSessionRole::Controller,
            server_id: Some("server-a".to_string()),
            controller_id: Some("controller-a".to_string()),
            broker_url: None,
            remote_daemon_address: Some(format!("{endpoint}:4000")),
            local_port: Some(4000),
            local_url: Some(format!("http://{endpoint}:4000")),
            tunnel_pid: None,
            remote_daemon_pid: Some(42),
            remote_daemon_lease_id: Some(lease.to_string()),
            homeboy_version: "test".to_string(),
            homeboy_build_identity: Some(format!("homeboy test+{lease}")),
            connected_at: "2026-07-20T00:00:00Z".to_string(),
            worker_identity: None,
            worker_pid: None,
            last_seen_at: None,
            leaseless_recovery_evidence: None,
        }
    }

    #[test]
    fn daemon_api_routes_persisted_a_and_b_job_operations_to_their_generation() {
        test_support::with_isolated_home(|_| {
            let a = session("lease-a", "daemon-a");
            let b = session("lease-b", "daemon-b");
            crate::generation_store::record_job("runner-a", &a, "job-a").expect("record A job");
            crate::generation_store::activate(
                "runner-a",
                &a,
                "build-b".to_string(),
                b.clone(),
                &["job-a".to_string()],
            )
            .expect("activate B");
            crate::generation_store::record_job("runner-a", &b, "job-b").expect("record B job");

            assert_eq!(
                daemon_api_session_for_path("runner-a", "/jobs/job-a/events", b.clone())
                    .expect("route A operation"),
                a
            );
            assert_eq!(
                daemon_api_session_for_path("runner-a", "/jobs/job-b", b.clone())
                    .expect("route B operation"),
                b
            );
        });
    }

    #[test]
    fn reverse_broker_body_normalizes_to_daemon_data_contract() {
        let data = reverse_broker_daemon_data(json!({ "job": { "id": "job-1" } }));
        let body = canonical_daemon_body(&data, "reverse broker job").expect("canonical body");
        assert_eq!(body["job"]["id"], "job-1");
    }
}

pub(super) fn daemon_post(client: &Client, local_url: &str, path: &str) -> Result<Value> {
    let response = daemon_post_json_text(
        client,
        local_url,
        path,
        &json!({}),
        DaemonPostOptions::default(),
    )?;
    let status_code = response.status_code;
    let body = response.body;
    let envelope: DaemonEnvelope =
        parse_daemon_response_json(&body, status_code, path, "parse daemon response")?;
    if !envelope.success {
        return Err(Error::internal_unexpected(format!(
            "daemon request failed: {}",
            envelope.error.unwrap_or(Value::Null)
        )));
    }
    envelope
        .data
        .ok_or_else(|| Error::internal_unexpected("daemon response missing data"))
}

pub(super) fn daemon_post_json_text(
    client: &Client,
    local_url: &str,
    path: &str,
    payload: &Value,
    options: DaemonPostOptions,
) -> Result<DaemonHttpTextResponse> {
    let request = client
        .post(format!("{}{}", local_url.trim_end_matches('/'), path))
        .json(payload);
    let response = with_daemon_post_options(request, options)
        .send()
        .map_err(|err| {
            daemon_transport_error(
                classify_reqwest_error(&err),
                path,
                err.status().map(|status| status.as_u16()),
                "query runner daemon",
                err.to_string(),
            )
        })?;
    let status_code = response.status().as_u16();
    let body = response.text().map_err(|err| {
        daemon_transport_error(
            if err.is_timeout() {
                DaemonHttpErrorKind::Timeout
            } else {
                DaemonHttpErrorKind::BodyDecode
            },
            path,
            Some(status_code),
            "read runner daemon response",
            err.to_string(),
        )
    })?;

    Ok(DaemonHttpTextResponse { status_code, body })
}
