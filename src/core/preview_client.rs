use base64::Engine;
use reqwest::blocking::Client;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde::de::{self, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::json;
use std::collections::BTreeMap;
use std::io::Read;
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
    pub session_id: Option<String>,
    pub token_env: String,
    pub poll_timeout_secs: u64,
    pub ready_stdout: bool,
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

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PreviewClientAuthDiagnostic {
    pub command: &'static str,
    pub token_env: String,
    pub token_present: bool,
    pub token_empty: bool,
    pub local_token_sha256: Option<String>,
    pub expected_sha256_env: String,
    pub expected_sha256: Option<String>,
    pub matches_expected: Option<bool>,
    pub hashing_semantics: &'static str,
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
    #[serde(default, deserialize_with = "deserialize_response_headers")]
    pub headers: Vec<(String, String)>,
    #[serde(default)]
    pub body_base64: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub body_stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<PreviewClientForwardError>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PreviewIngressResponseChunk {
    pub request_id: String,
    pub sequence: u64,
    pub body_base64: String,
    #[serde(default)]
    pub complete: bool,
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
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|err| {
            Error::internal_unexpected(format!("build local-origin HTTP client: {err}"))
        })?;

    register_session(&client, &spec, &token)?;
    if spec.ready_stdout {
        println!("ready https://{}", spec.public_host);
    }
    while !stop.load(Ordering::SeqCst) {
        match poll_next_request(&client, &spec, &token) {
            Ok(Some(request)) => {
                let response_client = client.clone();
                let local_client = local_client.clone();
                let worker_spec = spec.clone();
                let worker_token = token.clone();
                thread::spawn(move || {
                    if let Err(err) = forward_to_local_origin_streaming(
                        &local_client,
                        &response_client,
                        &worker_spec,
                        &worker_token,
                        request,
                    ) {
                        eprintln!(
                            "{}",
                            json!({
                                "command": "tunnel.preview_client.start",
                                "event": "response_failed",
                                "public_host": worker_spec.public_host,
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

pub fn diagnose_auth(
    token_env: &str,
    expected_sha256_env: &str,
) -> Result<PreviewClientAuthDiagnostic> {
    require_non_empty("token_env", token_env)?;
    require_non_empty("expected_sha256_env", expected_sha256_env)?;
    let token = std::env::var(token_env).ok();
    let expected = std::env::var(expected_sha256_env)
        .ok()
        .filter(|value| !value.trim().is_empty());
    let local = token
        .as_ref()
        .filter(|value| !value.is_empty())
        .map(|value| sha256_hex(value.as_bytes()));
    Ok(PreviewClientAuthDiagnostic {
        command: "tunnel.preview_client.diagnose_auth",
        token_env: token_env.to_string(),
        token_present: token.is_some(),
        token_empty: token.as_ref().is_some_and(|value| value.is_empty()),
        local_token_sha256: local.clone(),
        expected_sha256_env: expected_sha256_env.to_string(),
        expected_sha256: expected.clone(),
        matches_expected: local
            .zip(expected)
            .map(|(local, expected)| local.eq_ignore_ascii_case(&expected)),
        hashing_semantics:
            "sha256 over exact token bytes; shell equivalent: printf %s \"$TOKEN\" | sha256sum",
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
            headers: vec![("content-type".to_string(), "application/json".to_string())],
            body_base64: base64::engine::general_purpose::STANDARD.encode(
                json!({
                    "error": error.kind,
                    "message": error.message,
                })
                .to_string(),
            ),
            body_stream: false,
            error: Some(error),
        },
    }
}

fn forward_to_local_origin_streaming(
    local_client: &Client,
    response_client: &Client,
    spec: &PreviewClientStartSpec,
    token: &str,
    request: PreviewIngressRequest,
) -> Result<()> {
    if request.method.eq_ignore_ascii_case("OPTIONS") {
        let response = forward_to_local_origin(local_client, &spec.local_origin, request);
        return send_response(response_client, spec, token, &response);
    }

    match open_local_origin_response(local_client, &spec.local_origin, &request) {
        Ok((status, headers, mut response)) => {
            send_response(
                response_client,
                spec,
                token,
                &PreviewIngressResponse {
                    request_id: request.request_id.clone(),
                    status,
                    headers,
                    body_base64: String::new(),
                    body_stream: true,
                    error: None,
                },
            )?;

            let mut sequence = 0_u64;
            let mut buffer = vec![0_u8; 64 * 1024];
            loop {
                let read = response.read(&mut buffer).map_err(|err| {
                    Error::internal_unexpected(format!("read local-origin response body: {err}"))
                })?;
                if read == 0 {
                    return send_response_chunk(
                        response_client,
                        spec,
                        token,
                        &PreviewIngressResponseChunk {
                            request_id: request.request_id,
                            sequence,
                            body_base64: String::new(),
                            complete: true,
                        },
                    );
                }
                send_response_chunk(
                    response_client,
                    spec,
                    token,
                    &PreviewIngressResponseChunk {
                        request_id: request.request_id.clone(),
                        sequence,
                        body_base64: base64::engine::general_purpose::STANDARD
                            .encode(&buffer[..read]),
                        complete: false,
                    },
                )?;
                sequence += 1;
            }
        }
        Err(error) => send_response(
            response_client,
            spec,
            token,
            &PreviewIngressResponse {
                request_id: request.request_id,
                status: 502,
                headers: vec![("content-type".to_string(), "application/json".to_string())],
                body_base64: base64::engine::general_purpose::STANDARD.encode(
                    json!({
                        "error": error.kind,
                        "message": error.message,
                    })
                    .to_string(),
                ),
                body_stream: false,
                error: Some(error),
            },
        ),
    }
}

fn forward_to_local_origin_result(
    client: &Client,
    local_origin: &str,
    request: &PreviewIngressRequest,
) -> std::result::Result<PreviewIngressResponse, PreviewClientForwardError> {
    if request.method.eq_ignore_ascii_case("OPTIONS") {
        return Ok(PreviewIngressResponse {
            request_id: request.request_id.clone(),
            status: 204,
            headers: cors_headers(Vec::new(), &request.path),
            body_base64: base64::engine::general_purpose::STANDARD.encode([]),
            body_stream: false,
            error: None,
        });
    }
    let (status, headers, response) = open_local_origin_response(client, local_origin, request)?;
    let body = response.bytes().map_err(|err| PreviewClientForwardError {
        kind: "local_origin_response_failed".to_string(),
        message: err.to_string(),
    })?;
    Ok(PreviewIngressResponse {
        request_id: request.request_id.clone(),
        status,
        headers,
        body_base64: base64::engine::general_purpose::STANDARD.encode(body),
        body_stream: false,
        error: None,
    })
}

fn open_local_origin_response(
    client: &Client,
    local_origin: &str,
    request: &PreviewIngressRequest,
) -> std::result::Result<
    (u16, Vec<(String, String)>, reqwest::blocking::Response),
    PreviewClientForwardError,
> {
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
    let headers = cors_headers(response_headers(response.headers()), &request.path);
    Ok((status, headers, response))
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
            "session_id": spec.session_id,
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

fn send_response_chunk(
    client: &Client,
    spec: &PreviewClientStartSpec,
    token: &str,
    chunk: &PreviewIngressResponseChunk,
) -> Result<()> {
    post_json(
        client,
        spec,
        token,
        "/preview/client/respond-chunk",
        json!({
            "public_host": spec.public_host,
            "chunk": chunk,
        }),
        "send preview client response chunk",
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
        let hint = if status.as_u16() == 401 {
            " Hint: run `homeboy tunnel preview-client diagnose-auth` and compare no-newline SHA-256 digests without printing token material."
        } else {
            ""
        };
        return Err(Error::internal_unexpected(format!(
            "{context} failed with HTTP {}: {}{}",
            status.as_u16(),
            text,
            hint
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

fn response_headers(headers: &HeaderMap) -> Vec<(String, String)> {
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

fn cors_headers(mut headers: Vec<(String, String)>, path: &str) -> Vec<(String, String)> {
    push_header_if_missing(&mut headers, "access-control-allow-origin", "*");
    push_header_if_missing(
        &mut headers,
        "access-control-allow-methods",
        "GET, HEAD, OPTIONS",
    );
    push_header_if_missing(&mut headers, "access-control-allow-headers", "*");
    if path.split('?').next().unwrap_or(path).ends_with(".json") {
        push_header_if_missing(&mut headers, "content-type", "application/json");
    }
    headers
}

fn push_header_if_missing(headers: &mut Vec<(String, String)>, name: &str, value: &str) {
    if !headers
        .iter()
        .any(|(header_name, _)| header_name.eq_ignore_ascii_case(name))
    {
        headers.push((name.to_string(), value.to_string()));
    }
}

fn deserialize_response_headers<'de, D>(
    deserializer: D,
) -> std::result::Result<Vec<(String, String)>, D::Error>
where
    D: Deserializer<'de>,
{
    struct ResponseHeadersVisitor;

    impl<'de> Visitor<'de> for ResponseHeadersVisitor {
        type Value = Vec<(String, String)>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("a header object or ordered [name, value] header pairs")
        }

        fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
        where
            A: de::MapAccess<'de>,
        {
            let mut headers = Vec::new();
            while let Some((name, value)) = map.next_entry::<String, String>()? {
                headers.push((name, value));
            }
            Ok(headers)
        }

        fn visit_seq<A>(self, mut seq: A) -> std::result::Result<Self::Value, A::Error>
        where
            A: SeqAccess<'de>,
        {
            let mut headers = Vec::new();
            while let Some((name, value)) = seq.next_element::<(String, String)>()? {
                headers.push((name, value));
            }
            Ok(headers)
        }
    }

    deserializer.deserialize_any(ResponseHeadersVisitor)
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    format!("{digest:x}")
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
            format!("preview client local origin must be a valid HTTP(S) URL: {err}"),
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
            session_id: Some("run-1".to_string()),
            token_env: "HOMEBOY_PREVIEW_TUNNEL_TOKEN".to_string(),
            poll_timeout_secs: 30,
            ready_stdout: false,
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
        assert_eq!(
            header_value(&response.headers, "content-type"),
            Some("text/plain")
        );
        assert_eq!(
            header_value(&response.headers, "x-origin"),
            Some("local-service")
        );
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

    #[test]
    fn options_preflight_returns_cors_without_local_origin() {
        let client = Client::builder()
            .timeout(Duration::from_millis(100))
            .build()
            .expect("client");
        let response = forward_to_local_origin(
            &client,
            "http://127.0.0.1:9",
            PreviewIngressRequest {
                request_id: "req-options".to_string(),
                method: "OPTIONS".to_string(),
                path: "/homeboy/workflow-bench/runs/run/artifacts/blueprint.after.json".to_string(),
                headers: BTreeMap::new(),
                body_base64: None,
            },
        );

        assert_eq!(response.status, 204);
        assert_eq!(
            header_value(&response.headers, "access-control-allow-origin"),
            Some("*")
        );
        assert_eq!(
            header_value(&response.headers, "content-type"),
            Some("application/json")
        );
    }

    #[test]
    fn forwards_redirect_location_and_multiple_set_cookie_headers_without_following() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind local origin");
        let port = listener.local_addr().expect("local addr").port();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept request");
            let mut buffer = [0_u8; 4096];
            let read = stream.read(&mut buffer).expect("read request");
            let request = String::from_utf8_lossy(&buffer[..read]);
            assert!(request.starts_with("GET /__reviewer/auth-bootstrap?token=redacted HTTP/1.1"));
            stream
                .write_all(
                    b"HTTP/1.1 302 Found\r\nLocation: /admin/\r\nSet-Cookie: reviewer_auth=one; Path=/; HttpOnly\r\nSet-Cookie: reviewer_test_cookie=Cookie%20check; Path=/\r\nContent-Length: 0\r\n\r\n",
                )
                .expect("write response");
        });
        let client = Client::builder()
            .timeout(Duration::from_secs(5))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("client");

        let response = forward_to_local_origin(
            &client,
            &format!("http://127.0.0.1:{port}"),
            PreviewIngressRequest {
                request_id: "req-bootstrap".to_string(),
                method: "GET".to_string(),
                path: "/__reviewer/auth-bootstrap?token=redacted".to_string(),
                headers: BTreeMap::new(),
                body_base64: None,
            },
        );

        server.join().expect("server finished");
        assert_eq!(response.status, 302);
        assert_eq!(header_value(&response.headers, "location"), Some("/admin/"));
        let cookies = header_values(&response.headers, "set-cookie");
        assert_eq!(cookies.len(), 2);
        assert!(cookies[0].starts_with("reviewer_auth="));
        assert!(cookies[1].starts_with("reviewer_test_cookie="));
        assert!(response.error.is_none());
    }

    #[test]
    fn auth_diagnostic_hashes_exact_token_bytes() {
        std::env::set_var("HOMEBOY_TEST_PREVIEW_TOKEN", "abc");
        std::env::set_var(
            "HOMEBOY_TEST_PREVIEW_TOKEN_SHA256",
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        );

        let diagnostic = diagnose_auth(
            "HOMEBOY_TEST_PREVIEW_TOKEN",
            "HOMEBOY_TEST_PREVIEW_TOKEN_SHA256",
        )
        .expect("diagnostic");

        assert_eq!(diagnostic.local_token_sha256, diagnostic.expected_sha256);
        assert_eq!(diagnostic.matches_expected, Some(true));

        std::env::remove_var("HOMEBOY_TEST_PREVIEW_TOKEN");
        std::env::remove_var("HOMEBOY_TEST_PREVIEW_TOKEN_SHA256");
    }

    fn header_value<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
        headers
            .iter()
            .find(|(header_name, _)| header_name.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    fn header_values<'a>(headers: &'a [(String, String)], name: &str) -> Vec<&'a str> {
        headers
            .iter()
            .filter(|(header_name, _)| header_name.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
            .collect()
    }
}
