use reqwest::blocking::Client;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::error::{Error, ErrorCode, Result};

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
        .map_err(|err| Error::internal_unexpected(format!("query runner daemon: {err}")))?;
    let status_code = response.status().as_u16();
    let body = response
        .text()
        .map_err(|err| Error::internal_unexpected(format!("read runner daemon response: {err}")))?;
    let envelope: DaemonGetEnvelope =
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
