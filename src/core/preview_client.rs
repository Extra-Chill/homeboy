use base64::Engine;
use reqwest::blocking::Client;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use crate::core::{Error, Result};

#[derive(Debug, Clone)]
pub struct PreviewClientStartSpec {
    pub ingress: String,
    pub public_host: String,
    pub local_origin: String,
    pub token_env: String,
    pub poll_timeout_secs: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PreviewClientReport {
    pub command: &'static str,
    pub ingress: String,
    pub public_host: String,
    pub local_origin: String,
    pub registered: bool,
    pub stopped: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PreviewIngressRequest {
    pub request_id: String,
    pub method: String,
    pub path: String,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_base64: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PreviewIngressNextResponse {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request: Option<PreviewIngressRequest>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PreviewIngressResponse {
    pub request_id: String,
    pub status: u16,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    pub body_base64: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<PreviewClientForwardError>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PreviewClientForwardError {
    pub kind: String,
    pub message: String,
}

pub fn start(spec: PreviewClientStartSpec) -> Result<PreviewClientReport> {
    validate_start_spec(&spec)?;
    let token = std::env::var(&spec.token_env).map_err(|_| {
        Error::validation_invalid_argument(
            "token_env",
            "preview client token environment variable is not set",
            Some(spec.token_env.clone()),
            None,
        )
    })?;
    if token.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "token_env",
            "preview client token environment variable is empty",
            Some(spec.token_env.clone()),
            None,
        ));
    }

    let stop = Arc::new(AtomicBool::new(false));
    install_shutdown_handler(stop.clone())?;
    let client = Client::builder()
        .timeout(Duration::from_secs(spec.poll_timeout_secs.max(1) + 5))
        .build()
        .map_err(|err| {
            Error::internal_unexpected(format!("build preview client HTTP client: {err}"))
        })?;
    let local_client = Client::builder()
        .timeout(Duration::from_secs(60))
        .build()
        .map_err(|err| {
            Error::internal_unexpected(format!("build local-origin HTTP client: {err}"))
        })?;

    register_session(&client, &spec, &token)?;
    while !stop.load(Ordering::SeqCst) {
        match poll_next_request(&client, &spec, &token) {
            Ok(Some(request)) => {
                let response_client = client.clone();
                let local_client = local_client.clone();
                let worker_spec = spec.clone();
                let worker_token = token.clone();
                thread::spawn(move || {
                    let response =
                        forward_to_local_origin(&local_client, &worker_spec.local_origin, request);
                    if let Err(err) =
                        send_response(&response_client, &worker_spec, &worker_token, &response)
                    {
                        eprintln!(
                            "{}",
                            json!({
                                "command": "tunnel.preview_client.start",
                                "event": "response_failed",
                                "public_host": worker_spec.public_host,
                                "request_id": response.request_id,
                                "error": err.message,
                            })
                        );
                    }
                });
            }
            Ok(None) => thread::sleep(Duration::from_millis(100)),
            Err(err) => {
                eprintln!(
                    "{}",
                    json!({
                        "command": "tunnel.preview_client.start",
                        "event": "poll_failed",
                        "public_host": spec.public_host,
                        "error": err.message,
                    })
                );
                thread::sleep(Duration::from_secs(1));
            }
        }
    }
    close_session(&client, &spec, &token)?;

    Ok(PreviewClientReport {
        command: "tunnel.preview_client.start",
        ingress: spec.ingress,
        public_host: spec.public_host,
        local_origin: spec.local_origin,
        registered: true,
        stopped: true,
    })
}

pub fn forward_to_local_origin(
    client: &Client,
    local_origin: &str,
    request: PreviewIngressRequest,
) -> PreviewIngressResponse {
    match forward_to_local_origin_result(client, local_origin, &request) {
        Ok(response) => response,
        Err(error) => PreviewIngressResponse {
            request_id: request.request_id,
            status: 502,
            headers: BTreeMap::from([("content-type".to_string(), "application/json".to_string())]),
            body_base64: base64::engine::general_purpose::STANDARD.encode(
                json!({
                    "error": error.kind,
                    "message": error.message,
                })
                .to_string(),
            ),
            error: Some(error),
        },
    }
}

fn forward_to_local_origin_result(
    client: &Client,
    local_origin: &str,
    request: &PreviewIngressRequest,
) -> std::result::Result<PreviewIngressResponse, PreviewClientForwardError> {
    let method = request
        .method
        .parse()
        .map_err(|err| PreviewClientForwardError {
            kind: "invalid_method".to_string(),
            message: format!("invalid ingress request method: {err}"),
        })?;
    let url = local_request_url(local_origin, &request.path)?;
    let body = decode_body(request.body_base64.as_deref())?;
    let mut local_request = client
        .request(method, url)
        .headers(forward_request_headers(&request.headers));
    if let Some(body) = body {
        local_request = local_request.body(body);
    }
    let response = local_request
        .send()
        .map_err(|err| PreviewClientForwardError {
            kind: "local_origin_request_failed".to_string(),
            message: err.to_string(),
        })?;
    let status = response.status().as_u16();
    let headers = response_headers(response.headers());
    let body = response.bytes().map_err(|err| PreviewClientForwardError {
        kind: "local_origin_response_failed".to_string(),
        message: err.to_string(),
    })?;
    Ok(PreviewIngressResponse {
        request_id: request.request_id.clone(),
        status,
        headers,
        body_base64: base64::engine::general_purpose::STANDARD.encode(body),
        error: None,
    })
}

fn register_session(client: &Client, spec: &PreviewClientStartSpec, token: &str) -> Result<()> {
    post_json(
        client,
        spec,
        token,
        "/preview/client/register",
        json!({
            "public_host": spec.public_host,
            "local_origin": spec.local_origin,
        }),
        "register preview client session",
    )
    .map(|_| ())
}

fn poll_next_request(
    client: &Client,
    spec: &PreviewClientStartSpec,
    token: &str,
) -> Result<Option<PreviewIngressRequest>> {
    let value = post_json(
        client,
        spec,
        token,
        "/preview/client/next",
        json!({
            "public_host": spec.public_host,
            "timeout_secs": spec.poll_timeout_secs.max(1),
        }),
        "poll preview client request",
    )?;
    let next: PreviewIngressNextResponse = serde_json::from_value(value).map_err(|err| {
        Error::internal_json(
            err.to_string(),
            Some("parse preview client next response".to_string()),
        )
    })?;
    Ok(next.request)
}

fn send_response(
    client: &Client,
    spec: &PreviewClientStartSpec,
    token: &str,
    response: &PreviewIngressResponse,
) -> Result<()> {
    post_json(
        client,
        spec,
        token,
        "/preview/client/respond",
        json!({
            "public_host": spec.public_host,
            "response": response,
        }),
        "send preview client response",
    )
    .map(|_| ())
}

fn close_session(client: &Client, spec: &PreviewClientStartSpec, token: &str) -> Result<()> {
    post_json(
        client,
        spec,
        token,
        "/preview/client/close",
        json!({
            "public_host": spec.public_host,
        }),
        "close preview client session",
    )
    .map(|_| ())
}

fn post_json(
    client: &Client,
    spec: &PreviewClientStartSpec,
    token: &str,
    path: &str,
    body: serde_json::Value,
    context: &str,
) -> Result<serde_json::Value> {
    let response = client
        .post(format!("{}{}", spec.ingress.trim_end_matches('/'), path))
        .bearer_auth(token)
        .json(&body)
        .send()
        .map_err(|err| Error::internal_unexpected(format!("{context}: {err}")))?;
    let status = response.status();
    let text = response
        .text()
        .map_err(|err| Error::internal_unexpected(format!("read {context} response: {err}")))?;
    if !status.is_success() {
        return Err(Error::internal_unexpected(format!(
            "{context} failed with HTTP {}: {}",
            status.as_u16(),
            text
        )));
    }
    if text.trim().is_empty() {
        return Ok(json!({}));
    }
    serde_json::from_str(&text)
        .map_err(|err| Error::internal_json(err.to_string(), Some(context.to_string())))
}

fn local_request_url(
    local_origin: &str,
    path: &str,
) -> std::result::Result<String, PreviewClientForwardError> {
    if !path.starts_with('/') {
        return Err(PreviewClientForwardError {
            kind: "invalid_path".to_string(),
            message: "preview ingress request path must start with /".to_string(),
        });
    }
    Ok(format!("{}{}", local_origin.trim_end_matches('/'), path))
}

fn decode_body(
    body_base64: Option<&str>,
) -> std::result::Result<Option<Vec<u8>>, PreviewClientForwardError> {
    body_base64
        .map(|body| {
            base64::engine::general_purpose::STANDARD
                .decode(body)
                .map_err(|err| PreviewClientForwardError {
                    kind: "invalid_body".to_string(),
                    message: format!("preview ingress request body is not valid base64: {err}"),
                })
        })
        .transpose()
}

fn forward_request_headers(headers: &BTreeMap<String, String>) -> HeaderMap {
    let mut forwarded = HeaderMap::new();
    for (name, value) in headers {
        let normalized = name.to_ascii_lowercase();
        if matches!(
            normalized.as_str(),
            "connection" | "host" | "content-length" | "transfer-encoding" | "upgrade"
        ) {
            continue;
        }
        if let (Ok(name), Ok(value)) = (
            HeaderName::from_bytes(name.as_bytes()),
            HeaderValue::from_str(value),
        ) {
            forwarded.insert(name, value);
        }
    }
    forwarded
}

fn response_headers(headers: &HeaderMap) -> BTreeMap<String, String> {
    headers
        .iter()
        .filter_map(|(name, value)| {
            let normalized = name.as_str().to_ascii_lowercase();
            if matches!(
                normalized.as_str(),
                "connection" | "transfer-encoding" | "upgrade"
            ) {
                return None;
            }
            value
                .to_str()
                .ok()
                .map(|value| (normalized, value.to_string()))
        })
        .collect()
}

fn validate_start_spec(spec: &PreviewClientStartSpec) -> Result<()> {
    require_non_empty("ingress", &spec.ingress)?;
    require_non_empty("public_host", &spec.public_host)?;
    require_non_empty("local_origin", &spec.local_origin)?;
    require_non_empty("token_env", &spec.token_env)?;
    if spec.public_host.contains('*') {
        return Err(Error::validation_invalid_argument(
            "public_host",
            "preview client must register exactly one public host, not a wildcard",
            Some(spec.public_host.clone()),
            None,
        ));
    }
    let parsed_origin = reqwest::Url::parse(&spec.local_origin).map_err(|err| {
        Error::validation_invalid_argument(
            "local_origin",
            &format!("preview client local origin must be a valid HTTP(S) URL: {err}"),
            Some(spec.local_origin.clone()),
            None,
        )
    })?;
    if !matches!(parsed_origin.scheme(), "http" | "https") {
        return Err(Error::validation_invalid_argument(
            "local_origin",
            "preview client local origin must use http or https",
            Some(spec.local_origin.clone()),
            Some(vec![
                "http://127.0.0.1:<port>".to_string(),
                "http://localhost:<port>".to_string(),
            ]),
        ));
    }
    Ok(())
}

fn require_non_empty(field: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            field,
            "value is required",
            None,
            None,
        ));
    }
    Ok(())
}

fn install_shutdown_handler(stop: Arc<AtomicBool>) -> Result<()> {
    ctrlc::set_handler(move || {
        stop.store(true, Ordering::SeqCst);
    })
    .map_err(|err| {
        Error::internal_unexpected(format!("install preview client signal handler: {err}"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    #[test]
    fn rejects_wildcard_public_host() {
        let err = validate_start_spec(&PreviewClientStartSpec {
            ingress: "https://preview.example.test".to_string(),
            public_host: "*-tunnel.example.test".to_string(),
            local_origin: "http://127.0.0.1:49822".to_string(),
            token_env: "HOMEBOY_PREVIEW_TUNNEL_TOKEN".to_string(),
            poll_timeout_secs: 30,
        })
        .expect_err("wildcard host should fail");

        assert_eq!(err.code, crate::core::ErrorCode::ValidationInvalidArgument);
        assert!(err.message.contains("exactly one public host"));
    }

    #[test]
    fn forwards_request_to_loopback_origin_and_serializes_response() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind local origin");
        let port = listener.local_addr().expect("local addr").port();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept request");
            let mut buffer = [0_u8; 4096];
            let read = stream.read(&mut buffer).expect("read request");
            let request = String::from_utf8_lossy(&buffer[..read]);
            assert!(request.starts_with("POST /assets/app.js?ver=1 HTTP/1.1"));
            assert!(request.contains("x-preview-test: yes"));
            assert!(request.ends_with("asset-body"));
            stream
                .write_all(
                    b"HTTP/1.1 201 Created\r\nContent-Type: text/plain\r\nX-Origin: local-service\r\nContent-Length: 2\r\n\r\nok",
                )
                .expect("write response");
        });
        let client = Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .expect("client");

        let response = forward_to_local_origin(
            &client,
            &format!("http://127.0.0.1:{port}"),
            PreviewIngressRequest {
                request_id: "req-1".to_string(),
                method: "POST".to_string(),
                path: "/assets/app.js?ver=1".to_string(),
                headers: BTreeMap::from([
                    ("Host".to_string(), "public.example.test".to_string()),
                    ("X-Preview-Test".to_string(), "yes".to_string()),
                ]),
                body_base64: Some(base64::engine::general_purpose::STANDARD.encode("asset-body")),
            },
        );

        server.join().expect("server finished");
        assert_eq!(response.request_id, "req-1");
        assert_eq!(response.status, 201);
        assert_eq!(response.headers["content-type"], "text/plain");
        assert_eq!(response.headers["x-origin"], "local-service");
        assert_eq!(
            base64::engine::general_purpose::STANDARD
                .decode(response.body_base64)
                .expect("decode response"),
            b"ok"
        );
        assert!(response.error.is_none());
    }

    #[test]
    fn local_origin_failure_is_reported_separately() {
        let client = Client::builder()
            .timeout(Duration::from_millis(100))
            .build()
            .expect("client");
        let response = forward_to_local_origin(
            &client,
            "http://127.0.0.1:9",
            PreviewIngressRequest {
                request_id: "req-fail".to_string(),
                method: "GET".to_string(),
                path: "/".to_string(),
                headers: BTreeMap::new(),
                body_base64: None,
            },
        );

        assert_eq!(response.status, 502);
        let error = response.error.expect("error");
        assert_eq!(error.kind, "local_origin_request_failed");
    }
}
