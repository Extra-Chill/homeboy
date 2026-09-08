use reqwest::blocking::Client;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{json, Value};

use homeboy_control_plane_contract::{ControlPlaneRun, RunId};
use homeboy_core::error::{Error, ErrorCode, Result};

/// Minimal CLI-style success envelope shared by the runner daemon HTTP GET
/// helper. Both `connection.rs` and `execution.rs` previously carried their own
/// byte-identical copy of this struct plus `daemon_get`; they now share this
/// single implementation (#5362).
#[derive(Debug, Clone, Deserialize)]
struct DaemonGetEnvelope {
    success: bool,
    data: Option<Value>,
    error: Option<Value>,
}

/// Issue a GET against a runner daemon's local URL, parse the canonical CLI
/// envelope, and return its `data` payload. Shared by the runner connection and
/// execution paths so the request/parse/validate logic lives in one place.
pub(super) fn daemon_get(client: &Client, local_url: &str, path: &str) -> Result<Value> {
    let response = client
        .get(format!("{}{}", local_url.trim_end_matches('/'), path))
        .send()
        .map_err(|err| {
            let mut error = Error::internal_unexpected(format!("query runner daemon: {err}"));
            error.details["request_timeout"] = json!(err.is_timeout());
            error
        })?;
    let status_code = response.status().as_u16();
    let body = response.text().map_err(|err| {
        let mut error = Error::internal_unexpected(format!("read runner daemon response: {err}"));
        error.details["request_timeout"] = json!(err.is_timeout());
        error
    })?;
    let envelope: DaemonGetEnvelope =
        parse_daemon_response_json(&body, status_code, path, "parse daemon response")?;
    if !envelope.success {
        return Err(Error::new(
            ErrorCode::InternalUnexpected,
            format!(
                "daemon request failed: {}",
                envelope.error.unwrap_or(Value::Null)
            ),
            json!({ "http_status": status_code, "path": path }),
        ));
    }
    envelope
        .data
        .ok_or_else(|| Error::internal_unexpected("daemon response missing data"))
}

/// Read one canonical run through the daemon transport using the shared
/// control-plane response type.
pub(super) fn control_plane_run(
    client: &Client,
    local_url: &str,
    run_id: &RunId,
) -> Result<ControlPlaneRun> {
    let path = format!("/v1/control-plane/runs/{run_id}");
    let value = daemon_get(client, local_url, &path)?;
    serde_json::from_value(value).map_err(|error| {
        Error::new(
            ErrorCode::InternalJsonError,
            "Invalid control-plane run response",
            json!({
                "error": error.to_string(),
                "path": path,
            }),
        )
    })
}

pub(crate) fn parse_daemon_response_json<T: DeserializeOwned>(
    body: &str,
    status_code: u16,
    path: &str,
    context: &str,
) -> Result<T> {
    serde_json::from_str(body)
        .map_err(|err| daemon_response_json_error(err, body, status_code, path, context))
}

fn daemon_response_json_error(
    err: serde_json::Error,
    body: &str,
    status_code: u16,
    path: &str,
    context: &str,
) -> Error {
    let trimmed = body.trim();
    let preview = trimmed.chars().take(500).collect::<String>();
    let likely_truncated =
        err.is_eof() || trimmed.ends_with('{') || trimmed.ends_with('[') || trimmed.ends_with(',');
    let mut error = Error::new(
        ErrorCode::InternalJsonError,
        "Malformed runner daemon JSON response",
        json!({
            "error": err.to_string(),
            "context": context,
            "http_status": status_code,
            "path": path,
            "body_bytes": body.len(),
            "body_preview": preview,
            "likely_truncated": likely_truncated,
            "daemon_transport_error": {
                "kind": "body_decode",
                "path": path,
                "http_status": status_code,
            },
        }),
    );
    error.retryable = Some(true);
    error
        .with_hint("The runner daemon response was malformed or truncated after a runner job may already exist; inspect the known job/run from the wrapping error instead of retrying blindly.".to_string())
        .with_hint("Reconnect the runner daemon if repeated reads keep returning malformed JSON.".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use homeboy_control_plane_contract::ControlPlaneRunState;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    #[test]
    fn external_client_reads_typed_control_plane_run_over_http() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
        let address = listener.local_addr().expect("address");
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("request");
            let mut request = [0_u8; 2048];
            let bytes = stream.read(&mut request).expect("read request");
            let request = String::from_utf8_lossy(&request[..bytes]);
            assert!(request.starts_with("GET /v1/control-plane/runs/run-13697 HTTP/1.1"));

            let mut run = ControlPlaneRun::new(RunId::new("run-13697").expect("run id"));
            run.state = ControlPlaneRunState::Succeeded;
            run.created_at = "2026-09-08T00:00:00Z".to_string();
            let body = serde_json::json!({
                "success": true,
                "data": run,
            })
            .to_string();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .expect("response");
        });

        let client = Client::builder().no_proxy().build().expect("HTTP client");
        let run = control_plane_run(
            &client,
            &format!("http://{address}"),
            &RunId::new("run-13697").expect("run id"),
        )
        .expect("control-plane run");
        server.join().expect("server");

        assert_eq!(run.run.as_str(), "run-13697");
        assert_eq!(run.state, ControlPlaneRunState::Succeeded);
    }
}
