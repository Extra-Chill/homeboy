use reqwest::blocking::{Client, RequestBuilder};
use serde::Deserialize;
use serde_json::{json, Value};

use homeboy_core::broker_auth::BROKER_TOKEN_HEADER;
use homeboy_core::error::{Error, Result};

#[derive(Debug, Deserialize)]
struct BrokerEnvelope {
    success: bool,
    data: Option<Value>,
    error: Option<Value>,
}

/// Attach the paired broker bearer token, when present, to an outgoing broker
/// request. Sent via both the canonical header and `Authorization: Bearer` so
/// the request works through proxies that strip one or the other.
pub(crate) fn with_broker_token(builder: RequestBuilder, token: Option<&str>) -> RequestBuilder {
    match token {
        Some(token) if !token.trim().is_empty() => builder
            .header(BROKER_TOKEN_HEADER, token)
            .bearer_auth(token),
        _ => builder,
    }
}

pub(crate) fn post_json(
    client: &Client,
    base_url: &str,
    path: &str,
    body: Value,
    action: &str,
    token: Option<&str>,
) -> Result<Value> {
    let response = with_broker_token(
        client
            .post(format!("{}{}", base_url.trim_end_matches('/'), path))
            .json(&body),
        token,
    )
    .send()
    .map_err(|err| broker_transport_error(action, err))?;
    let status_code = response.status().as_u16();
    let envelope: BrokerEnvelope = response.json().map_err(broker_response_error)?;
    if status_code >= 400 || !envelope.success {
        return Err(broker_wire_error(
            envelope.error.unwrap_or(Value::Null),
            Some(status_code),
            path,
        ));
    }
    let data = envelope
        .data
        .ok_or_else(|| Error::internal_unexpected("broker response missing data"))?;
    canonical_broker_body(&data)
}

pub(crate) fn get_json(
    client: &Client,
    base_url: &str,
    path: &str,
    action: &str,
    token: Option<&str>,
) -> Result<Value> {
    let response = with_broker_token(
        client.get(format!("{}{}", base_url.trim_end_matches('/'), path)),
        token,
    )
    .send()
    .map_err(|err| broker_transport_error(action, err))?;
    let status_code = response.status().as_u16();
    let envelope: BrokerEnvelope = response.json().map_err(broker_response_error)?;
    if status_code >= 400 || !envelope.success {
        return Err(broker_wire_error(
            envelope.error.unwrap_or(Value::Null),
            Some(status_code),
            path,
        ));
    }
    let data = envelope
        .data
        .ok_or_else(|| Error::internal_unexpected("broker response missing data"))?;
    canonical_broker_body(&data)
}

fn canonical_broker_body(data: &Value) -> Result<Value> {
    data.get("body")
        .cloned()
        .ok_or_else(|| Error::internal_unexpected("broker response missing canonical data.body"))
}

fn broker_wire_error(value: Value, status_code: Option<u16>, path: &str) -> Error {
    let mut error =
        crate::remote_error::from_wire(value, "broker request failed", status_code, path);
    if error.message != "broker request failed"
        && !error.message.starts_with("broker request failed: ")
    {
        error.message = format!("broker request failed: {}", error.message);
    }
    error
}

fn broker_transport_error(action: &str, err: reqwest::Error) -> Error {
    let mut error = Error::internal_unexpected(format!("{action}: {err}"));
    error.details["request_timeout"] = json!(request_error_is_timeout(&err));
    error
}

fn broker_response_error(err: reqwest::Error) -> Error {
    let mut error =
        Error::internal_json(err.to_string(), Some("parse broker response".to_string()));
    error.details["request_timeout"] = json!(request_error_is_timeout(&err));
    error
}

fn request_error_is_timeout(error: &reqwest::Error) -> bool {
    error.is_timeout() || error.to_string().to_ascii_lowercase().contains("timed out")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::time::Duration;

    #[test]
    fn canonical_broker_body_requires_data_body() {
        let err = canonical_broker_body(&json!({ "job": {} })).expect_err("reject legacy data");
        assert!(err.message.contains("data.body"));
    }

    #[test]
    fn canonical_broker_body_returns_nested_body() {
        let body =
            canonical_broker_body(&json!({ "body": { "job": { "id": "job-1" } } })).expect("body");
        assert_eq!(body["job"]["id"], "job-1");
    }

    #[test]
    fn get_json_preserves_timeout_when_headers_arrive_before_the_body_stalls() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
        let address = listener.local_addr().expect("address");
        // Two independent races made this test flaky under host contention
        // (homeboy#14984), both now removed rather than widened:
        //
        // 1. The server used to write response headers before reading any
        //    bytes of the client's request. Hyper writes the request and
        //    starts expecting a response as two separate steps; if the
        //    server's unread response bytes reach the socket while hyper is
        //    still mid-write (more likely once host scheduling delays widen
        //    that window), hyper reports `SendRequest(UnexpectedMessage)`
        //    instead of ever reaching the deliberate body stall this test
        //    means to exercise. Reading the request line first removes the
        //    interleaving entirely.
        // 2. The client's own timeout enforcement runs on reqwest's
        //    background runtime thread, which needs to be scheduled to
        //    notice the deadline before the server closes the socket, or the
        //    client observes a plain EOF instead of its own timeout. A
        //    server that stalls for a *fixed* duration races that scheduling
        //    against a wall clock. Holding the connection open on a signal
        //    the test only sends *after* it has already observed the
        //    client's timeout makes it structurally impossible for the
        //    server to close the socket before the client gives up.
        let (release_server, wait_for_release) = std::sync::mpsc::channel::<()>();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("request");
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .expect("read timeout");
            let mut request = Vec::new();
            let mut chunk = [0_u8; 4096];
            loop {
                let count = stream.read(&mut chunk).expect("read request");
                assert_ne!(count, 0, "client closed before sending its request");
                request.extend_from_slice(&chunk[..count]);
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 128\r\nConnection: close\r\n\r\n")
                .expect("headers");
            stream.flush().expect("flush headers");
            // Bounded only as a safety net against a genuinely hung test.
            let _ = wait_for_release.recv_timeout(Duration::from_secs(30));
        });
        let client = Client::builder()
            .timeout(Duration::from_millis(50))
            .build()
            .expect("client");

        let error = get_json(
            &client,
            &format!("http://{address}"),
            "/jobs",
            "read stalled broker jobs",
            None,
        )
        .expect_err("stalled broker body must time out");
        release_server.send(()).expect("release stalled server");

        server.join().expect("server");
        assert_eq!(error.details["request_timeout"], true, "{error:#?}");
    }
}
