use reqwest::blocking::Client;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{json, Value};

use homeboy_core::error::{Error, ErrorCode, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DaemonHttpErrorKind {
    Connect,
    Timeout,
    Status,
    BodyDecode,
}

impl DaemonHttpErrorKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Connect => "connect",
            Self::Timeout => "timeout",
            Self::Status => "status",
            Self::BodyDecode => "body_decode",
        }
    }
}

pub(crate) fn daemon_transport_error(
    kind: DaemonHttpErrorKind,
    path: &str,
    status_code: Option<u16>,
    context: &str,
    error: impl Into<String>,
) -> Error {
    Error::new(
        ErrorCode::InternalUnexpected,
        format!("{context}: {}", error.into()),
        json!({
            "daemon_transport_error": {
                "kind": kind.as_str(),
                "path": path,
                "http_status": status_code,
            }
        }),
    )
    .with_retryable(true)
}

pub(crate) fn classify_reqwest_error(err: &reqwest::Error) -> DaemonHttpErrorKind {
    if err.is_timeout() {
        DaemonHttpErrorKind::Timeout
    } else if err.is_connect() {
        DaemonHttpErrorKind::Connect
    } else {
        DaemonHttpErrorKind::Status
    }
}

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
            let mut error = daemon_transport_error(
                classify_reqwest_error(&err),
                path,
                None,
                "query runner daemon",
                err.to_string(),
            );
            error.details["request_timeout"] = json!(err.is_timeout());
            error
        })?;
    let status_code = response.status().as_u16();
    let body = response.text().map_err(|err| {
        let mut error = daemon_transport_error(
            if err.is_timeout() {
                DaemonHttpErrorKind::Timeout
            } else {
                DaemonHttpErrorKind::BodyDecode
            },
            path,
            Some(status_code),
            "read runner daemon response",
            err.to_string(),
        );
        error.details["request_timeout"] = json!(err.is_timeout());
        error
    })?;
    let envelope: DaemonGetEnvelope =
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
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::time::Duration;

    #[test]
    fn refused_get_connection_has_structured_recoverable_evidence() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind endpoint");
        let address = listener.local_addr().expect("endpoint");
        drop(listener);
        let client = Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(1))
            .build()
            .unwrap();
        let error = daemon_get(&client, &format!("http://{address}"), "/jobs/accepted-job")
            .expect_err("endpoint is unavailable");
        assert_eq!(error.details["daemon_transport_error"]["kind"], "connect");
        assert_eq!(
            error.details["daemon_transport_error"]["path"],
            "/jobs/accepted-job"
        );
        assert_eq!(error.retryable, Some(true));
        assert!(crate::daemon_health::runner_daemon_health_failure(&error).is_some());
    }

    #[test]
    fn interrupted_get_body_retains_status_and_transport_classification() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind endpoint");
        let address = listener.local_addr().expect("endpoint");
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept request");
            stream
                .set_read_timeout(Some(Duration::from_secs(1)))
                .unwrap();
            let mut request = [0; 4096];
            stream.read(&mut request).expect("read request");
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\n{")
                .expect("partial response");
        });
        let client = Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(1))
            .build()
            .unwrap();
        let error = daemon_get(&client, &format!("http://{address}"), "/jobs/accepted-job")
            .expect_err("body was interrupted");
        server.join().expect("server exits");
        assert_eq!(
            error.details["daemon_transport_error"]["kind"],
            "body_decode"
        );
        assert_eq!(error.details["daemon_transport_error"]["http_status"], 200);
        assert_eq!(error.retryable, Some(true));
        assert!(crate::daemon_health::runner_daemon_health_failure(&error).is_some());
    }
}
