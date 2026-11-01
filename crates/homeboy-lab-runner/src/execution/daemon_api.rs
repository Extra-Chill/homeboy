use std::time::Duration;

use reqwest::blocking::Client;
use reqwest::blocking::RequestBuilder;
use reqwest::header::CONNECTION;
use serde_json::{json, Value};

use homeboy_core::error::{Error, Result};

use super::super::broker_http;
use super::super::daemon_http_get::{
    classify_reqwest_error, daemon_get, daemon_transport_error, parse_daemon_response_json,
    DaemonHttpErrorKind,
};
use super::super::{load, RunnerSession};
use crate::connection::{ensure_transport, TransportLifetime};

#[allow(unused_imports)]
use super::*;

fn unsupported_daemon_api_method(method: &str) -> Error {
    Error::internal_unexpected(format!("unsupported daemon API method {method}"))
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
    daemon_api_get_for_session_with_timeout(session, path, Duration::from_secs(10))
}

/// Query a known daemon generation under the caller's remaining wall-clock
/// budget rather than allocating a fresh per-request timeout.
pub(crate) fn daemon_api_get_for_session_with_timeout(
    session: &RunnerSession,
    path: &str,
    timeout: Duration,
) -> Result<Value> {
    let local_url = session.local_url.as_deref().ok_or_else(|| {
        Error::internal_unexpected("known daemon generation has no direct local endpoint")
    })?;
    let client = Client::builder()
        .no_proxy()
        .timeout(timeout)
        .build()
        .map_err(|err| Error::internal_unexpected(format!("build daemon HTTP client: {err}")))?;
    daemon_get(&client, local_url, path)
}

pub(crate) fn daemon_api_post_for_session(session: &RunnerSession, path: &str) -> Result<Value> {
    let local_url = session.local_url.as_deref().ok_or_else(|| {
        Error::internal_unexpected("known daemon generation has no direct local endpoint")
    })?;
    let client = Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|err| Error::internal_unexpected(format!("build daemon HTTP client: {err}")))?;
    daemon_post(&client, local_url, path, &json!({}))
}

/// Total deadline for small control-plane daemon calls (status, capabilities,
/// job queries). Payload-carrying requests such as sealed source staging must
/// not use it; see [`daemon_api_post_json_for_session_with_timeout`].
pub(super) const DAEMON_CONTROL_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Post to a direct runner daemon with the paired broker token.
pub(crate) fn daemon_api_post_json_for_session_with_broker_token(
    session: &RunnerSession,
    path: &str,
    payload: &Value,
    broker_token: Option<&str>,
) -> Result<Value> {
    daemon_api_post_json_for_session_with_timeout(
        session,
        path,
        payload,
        broker_token,
        DAEMON_CONTROL_REQUEST_TIMEOUT,
    )
}

/// POST a JSON payload to a direct daemon session with an explicit total
/// deadline. Callers that upload large bodies (sealed source staging) pass a
/// deadline sized to the payload; a fixed 10s deadline made every Lab Cook for
/// a mid-size repository time out before the provider started
/// (Extra-Chill/homeboy#15863).
pub(crate) fn daemon_api_post_json_for_session_with_timeout(
    session: &RunnerSession,
    path: &str,
    payload: &Value,
    broker_token: Option<&str>,
    timeout: Duration,
) -> Result<Value> {
    let local_url = session.local_url.as_deref().ok_or_else(|| {
        Error::internal_unexpected("known daemon generation has no direct local endpoint")
    })?;
    let client = Client::builder()
        .no_proxy()
        .connect_timeout(DAEMON_CONTROL_REQUEST_TIMEOUT.min(timeout))
        .timeout(timeout)
        .build()
        .map_err(|err| Error::internal_unexpected(format!("build daemon HTTP client: {err}")))?;
    let request = broker_http::with_broker_token(
        client
            .post(format!("{}{}", local_url.trim_end_matches('/'), path))
            .json(payload),
        broker_token,
    );
    let response = daemon_post_json_request_text(request, path, DaemonPostOptions::default())?;
    let envelope: DaemonEnvelope = parse_daemon_response_json(
        &response.body,
        response.status_code,
        path,
        "parse daemon response",
    )?;
    if !envelope.success {
        return Err(crate::remote_error::from_wire(
            envelope.error.unwrap_or(Value::Null),
            "daemon request failed",
            Some(response.status_code),
            path,
        ));
    }
    envelope
        .data
        .ok_or_else(|| Error::internal_unexpected("daemon response missing data"))
}

pub fn daemon_api_post(runner_id: &str, path: &str) -> Result<Value> {
    daemon_api_request(runner_id, path, "POST")
}

pub(super) fn daemon_api_request(runner_id: &str, path: &str, method: &str) -> Result<Value> {
    let runner = load(runner_id)?;
    // Every job, run and artifact routes through the runner's one current session.
    // Transport health, not the full diagnostic report, decides whether to reattach.
    let session = ensure_transport(runner_id, TransportLifetime::Persisted).map_err(|_| {
        Error::validation_invalid_argument(
            "runner",
            "runner is not connected to a daemon; run `homeboy runner connect <runner-id>` first",
            Some(runner.id.clone()),
            Some(vec![
                "Read/query integrations use the connected daemon so results come from the runner machine.".to_string(),
            ]),
        )
    })?;
    let client = Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|err| Error::internal_unexpected(format!("build daemon HTTP client: {err}")))?;
    if let Some(local_url) = session.local_url.as_deref() {
        return match method {
            "GET" => daemon_get(&client, local_url, path),
            "POST" => daemon_post(&client, local_url, path, &json!({})),
            _ => Err(unsupported_daemon_api_method(method)),
        };
    }
    Err(Error::validation_invalid_argument(
        "runner",
        "runner session does not expose a local daemon URL",
        Some(runner.id),
        Some(vec![
            "Reconnect the runner with `homeboy runner connect <runner-id>` before querying runner jobs.".to_string(),
        ]),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    #[test]
    fn daemon_post_preserves_structured_remote_storage_error() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind daemon fixture");
        let address = listener.local_addr().expect("daemon fixture address");
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept daemon request");
            let mut request = [0_u8; 4096];
            let _ = stream.read(&mut request).expect("read daemon request");
            let body = serde_json::json!({
                "success": false,
                "error": {
                    "error": "internal.json_error",
                    "message": "staging record schema is invalid",
                    "details": {"phase": "lab_staging_submission"},
                    "hints": [{"message": "repair the staging record"}]
                }
            })
            .to_string();
            write!(
                stream,
                "HTTP/1.1 500 Internal Server Error\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .expect("write daemon response");
        });
        let client = Client::builder()
            .no_proxy()
            .build()
            .expect("daemon fixture client");

        let error = daemon_post(
            &client,
            &format!("http://{address}"),
            "/runner/staging/submit",
            &json!({}),
        )
        .expect_err("typed daemon error");
        server.join().expect("daemon fixture server");

        assert_eq!(
            error.code,
            homeboy_core::error::ErrorCode::InternalJsonError
        );
        assert_eq!(error.details["phase"], "lab_staging_submission");
        assert_eq!(error.hints[0].message, "repair the staging record");
    }
}

pub(super) fn daemon_post(
    client: &Client,
    local_url: &str,
    path: &str,
    payload: &Value,
) -> Result<Value> {
    let response = daemon_post_json_text(
        client,
        local_url,
        path,
        payload,
        DaemonPostOptions::default(),
    )?;
    let status_code = response.status_code;
    let body = response.body;
    let envelope: DaemonEnvelope =
        parse_daemon_response_json(&body, status_code, path, "parse daemon response")?;
    if !envelope.success {
        return Err(crate::remote_error::from_wire(
            envelope.error.unwrap_or(Value::Null),
            "daemon request failed",
            Some(status_code),
            path,
        ));
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
    daemon_post_json_request_text(request, path, options)
}

fn daemon_post_json_request_text(
    request: RequestBuilder,
    path: &str,
    options: DaemonPostOptions,
) -> Result<DaemonHttpTextResponse> {
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
