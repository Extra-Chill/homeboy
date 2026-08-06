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
fn with_broker_token(builder: RequestBuilder, token: Option<&str>) -> RequestBuilder {
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
        return Err(Error::new(
            homeboy_core::ErrorCode::InternalUnexpected,
            format!(
                "broker request failed: {}",
                envelope.error.unwrap_or(Value::Null)
            ),
            json!({ "http_status": status_code, "path": path }),
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
        return Err(Error::internal_unexpected(format!(
            "broker request failed: {}",
            envelope.error.unwrap_or(Value::Null)
        )));
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

fn broker_transport_error(action: &str, err: reqwest::Error) -> Error {
    let mut error = Error::internal_unexpected(format!("{action}: {err}"));
    error.details["request_timeout"] = json!(err.is_timeout());
    error
}

fn broker_response_error(err: reqwest::Error) -> Error {
    let mut error =
        Error::internal_json(err.to_string(), Some("parse broker response".to_string()));
    error.details["request_timeout"] = json!(err.is_timeout());
    error
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Write;
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
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("request");
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 128\r\nConnection: close\r\n\r\n")
                .expect("headers");
            stream.flush().expect("flush headers");
            std::thread::sleep(Duration::from_millis(100));
        });
        let client = Client::builder()
            .timeout(Duration::from_millis(10))
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

        server.join().expect("server");
        assert_eq!(error.details["request_timeout"], true);
    }
}
