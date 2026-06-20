use reqwest::blocking::Client;
use serde::Deserialize;
use serde_json::Value;

use crate::core::error::{Error, Result};

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
    let envelope: DaemonGetEnvelope = response.json().map_err(|err| {
        Error::internal_json(err.to_string(), Some("parse daemon response".to_string()))
    })?;
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
