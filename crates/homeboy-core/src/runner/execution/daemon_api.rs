use std::time::Duration;

use reqwest::blocking::Client;
use reqwest::blocking::RequestBuilder;
use reqwest::header::CONNECTION;
use serde_json::{json, Value};

use crate::error::{Error, ErrorCode, Result};

use super::super::broker_http;
use super::super::daemon_http_get::{daemon_get, parse_daemon_response_json};
use super::super::{load, status, RunnerTunnelMode};

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

pub fn daemon_api_post(runner_id: &str, path: &str) -> Result<Value> {
    daemon_api_request(runner_id, path, "POST")
}

pub(super) fn daemon_api_request(runner_id: &str, path: &str, method: &str) -> Result<Value> {
    let runner = load(runner_id)?;
    let connected = status(runner_id)?;
    let Some(session) = connected.session.filter(|_| connected.connected) else {
        return Err(Error::validation_invalid_argument(
            "runner",
            "runner is not connected to a daemon; run `homeboy runner connect <runner-id>` first",
            Some(runner.id),
            Some(vec![
                "Read/query integrations use the connected daemon so results come from the runner machine.".to_string(),
            ]),
        ));
    };
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
        let broker_token = crate::broker_auth::broker_submit_token_for_runner(runner_id)?;
        return match method {
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
        };
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
