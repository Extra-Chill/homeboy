use base64::Engine;
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::core::error::{Error, Result};
use crate::core::preview_client::PreviewIngressRequest;

use super::http::{
    artifact_cors_headers, is_hop_by_hop_header, log_request, record_failure, write_diagnostic,
    write_json_response, write_preview_response, write_response, write_status_and_headers,
    write_streaming_preview_response,
};
use super::install::validate_serve_spec;
use super::routes::{
    classify_route, classify_runtime_host_state, normalize_public_host, route_for_host, status,
    status_with_failures,
};
use super::types::{
    PreviewClientSession, PreviewClientSessions, PreviewCloseRequest, PreviewIngressAuth,
    PreviewIngressFailure, PreviewIngressLogLine, PreviewIngressRoute, PreviewIngressRouteLifecycle,
    PreviewIngressServeSpec, PreviewNextRequest, PreviewRegisterRequest, PreviewRespondChunkRequest,
    PreviewRespondRequest,
};

pub fn serve(spec: PreviewIngressServeSpec) -> Result<super::types::PreviewIngressStatus> {
    validate_serve_spec(&spec)?;
    let listener = TcpListener::bind(&spec.bind)
        .map_err(|e| Error::internal_io(e.to_string(), Some(spec.bind.clone())))?;
    serve_listener(spec, listener)
}

pub(crate) fn serve_listener(
    spec: PreviewIngressServeSpec,
    listener: TcpListener,
) -> Result<super::types::PreviewIngressStatus> {
    let sessions = Arc::new(PreviewClientSessions::default());
    let auth = Arc::new(PreviewIngressAuth {
        token_sha256_env: spec.token_sha256_env.clone(),
        token_sha256: preview_token_sha256(&spec.token_sha256_env),
    });
    eprintln!(
        "homeboy preview ingress listening on {} for {} ({})",
        spec.bind, spec.domain, spec.public_host_pattern
    );

    let recent_failures = Arc::new(Mutex::new(Vec::<PreviewIngressFailure>::new()));
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(60))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| Error::internal_unexpected(e.to_string()))?;

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let client = client.clone();
                let recent_failures = Arc::clone(&recent_failures);
                let sessions = Arc::clone(&sessions);
                let auth = Arc::clone(&auth);
                thread::spawn(move || {
                    if let Err(error) =
                        handle_connection(stream, client, sessions, auth, recent_failures)
                    {
                        eprintln!(
                            "homeboy preview ingress connection error: {}",
                            error.message
                        );
                    }
                });
            }
            Err(error) => {
                return Err(Error::internal_io(error.to_string(), Some(spec.bind)));
            }
        }
    }

    status(
        Some(spec.bind),
        Some(spec.domain),
        Some(spec.public_host_pattern),
    )
}

fn handle_connection(
    mut stream: TcpStream,
    client: reqwest::blocking::Client,
    sessions: Arc<PreviewClientSessions>,
    auth: Arc<PreviewIngressAuth>,
    recent_failures: Arc<Mutex<Vec<PreviewIngressFailure>>>,
) -> Result<()> {
    let started = Instant::now();
    let request_id = uuid::Uuid::new_v4().to_string();
    let mut reader = BufReader::new(stream.try_clone().map_err(|e| {
        Error::internal_io(
            e.to_string(),
            Some("clone preview ingress stream".to_string()),
        )
    })?);
    let request = read_http_request(&mut reader)?;
    let host = request.host.clone().unwrap_or_default();
    let path = request.target.clone();

    if request.target.split('?').next() == Some("/_homeboy/preview-ingress/status") {
        let failures = recent_failures
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        let inspected_host =
            query_value(&request.target, "host").map(|host| normalize_public_host(&host));
        let inspected_state = inspected_host.as_ref().map(|host| {
            classify_runtime_host_state(host, &sessions, &failures)
                .unwrap_or_else(|| "missing_session".to_string())
        });
        let body = serde_json::to_vec_pretty(&status_with_failures(
            None,
            None,
            None,
            failures,
            inspected_host,
            inspected_state,
        )?)
        .map_err(|e| {
            Error::internal_json(e.to_string(), Some("preview ingress status".to_string()))
        })?;
        write_response(
            &mut stream,
            200,
            "OK",
            &[(&"content-type".to_string(), "application/json".to_string())],
            &body,
        )?;
        log_request(&PreviewIngressLogLine {
            request_id,
            host,
            path,
            status: 200,
            bytes: body.len(),
            duration_ms: started.elapsed().as_millis(),
            classification: "status".to_string(),
        });
        return Ok(());
    }

    if request.target.starts_with("/preview/client/") {
        return handle_client_api(&mut stream, request, &sessions, &auth, &recent_failures);
    }

    let Some(route) = route_for_host(&host)? else {
        return proxy_reverse_channel_request(
            &mut stream,
            request,
            request_id,
            host,
            path,
            started,
            sessions,
            recent_failures,
        );
    };

    match classify_route(&route) {
        PreviewIngressRouteLifecycle::Expired => {
            let failure = PreviewIngressFailure {
                request_id: request_id.clone(),
                host: host.clone(),
                path: path.clone(),
                status: 410,
                classification: "expired_session".to_string(),
                message: "Homeboy preview ingress route is expired".to_string(),
            };
            record_failure(&recent_failures, failure.clone());
            write_diagnostic(&mut stream, &failure, started)
        }
        PreviewIngressRouteLifecycle::Disconnected => {
            let failure = PreviewIngressFailure {
                request_id: request_id.clone(),
                host: host.clone(),
                path: path.clone(),
                status: 410,
                classification: "disconnected_session".to_string(),
                message: "Homeboy preview ingress route is disconnected".to_string(),
            };
            record_failure(&recent_failures, failure.clone());
            write_diagnostic(&mut stream, &failure, started)
        }
        PreviewIngressRouteLifecycle::Active => proxy_request(
            &mut stream,
            &client,
            &route,
            request,
            request_id,
            host,
            path,
            started,
            recent_failures,
        ),
    }
}

pub(crate) struct IngressHttpRequest {
    pub(crate) method: String,
    pub(crate) target: String,
    pub(crate) headers: Vec<(String, String)>,
    pub(crate) host: Option<String>,
    pub(crate) body: Vec<u8>,
}

fn read_http_request(reader: &mut BufReader<TcpStream>) -> Result<IngressHttpRequest> {
    let mut first_line = String::new();
    reader
        .read_line(&mut first_line)
        .map_err(|e| Error::internal_io(e.to_string(), Some("read request line".to_string())))?;
    let mut parts = first_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let target = parts.next().unwrap_or("/").to_string();
    if method.is_empty() {
        return Err(Error::validation_invalid_argument(
            "request",
            "HTTP request line is empty",
            None,
            None,
        ));
    }

    let mut headers = Vec::new();
    let mut host = None;
    let mut content_length = 0usize;
    loop {
        let mut line = String::new();
        reader
            .read_line(&mut line)
            .map_err(|e| Error::internal_io(e.to_string(), Some("read headers".to_string())))?;
        if line == "\r\n" || line == "\n" || line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.trim_end().split_once(':') {
            let name = name.trim().to_ascii_lowercase();
            let value = value.trim().to_string();
            if name == "host" {
                host = Some(
                    value
                        .split(':')
                        .next()
                        .unwrap_or_default()
                        .to_ascii_lowercase(),
                );
            }
            if name == "content-length" {
                content_length = value.parse().unwrap_or(0);
            }
            headers.push((name, value));
        }
    }

    let mut body = vec![0; content_length];
    if content_length > 0 {
        reader
            .read_exact(&mut body)
            .map_err(|e| Error::internal_io(e.to_string(), Some("read body".to_string())))?;
    }

    Ok(IngressHttpRequest {
        method,
        target,
        headers,
        host,
        body,
    })
}

fn handle_client_api(
    stream: &mut TcpStream,
    request: IngressHttpRequest,
    sessions: &Arc<PreviewClientSessions>,
    auth: &PreviewIngressAuth,
    recent_failures: &Arc<Mutex<Vec<PreviewIngressFailure>>>,
) -> Result<()> {
    if request.method != "POST" {
        return write_json_response(
            stream,
            405,
            json!({ "error": "method_not_allowed", "message": "preview client endpoints require POST" }),
        );
    }
    if !authorized_preview_client(&request, auth) {
        let failure = PreviewIngressFailure {
            request_id: uuid::Uuid::new_v4().to_string(),
            host: request.host.clone().unwrap_or_default(),
            path: request.target.clone(),
            status: 401,
            classification: "auth_failed_recently".to_string(),
            message: "preview client bearer token is missing or invalid; compare no-newline SHA-256 digests with `homeboy tunnel preview-client diagnose-auth`".to_string(),
        };
        record_failure(recent_failures, failure);
        return write_json_response(
            stream,
            401,
            json!({
                "error": "unauthorized",
                "classification": "auth_failed_recently",
                "message": "preview client bearer token is missing or invalid",
                "hint": "Run `homeboy tunnel preview-client diagnose-auth`; Homeboy hashes exact token bytes (printf %s), never newline-terminated input."
            }),
        );
    }

    match request.target.as_str() {
        "/preview/client/register" => {
            let body: PreviewRegisterRequest = parse_json_body(&request.body, "register")?;
            let public_host = normalize_public_host(&body.public_host);
            validate_client_public_host(&public_host)?;
            validate_client_local_origin(&body.local_origin)?;
            let _session_id = body.session_id.unwrap_or_else(|| public_host.clone());
            let mut sessions_guard = sessions
                .sessions
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            sessions_guard.insert(
                public_host,
                PreviewClientSession {
                    local_origin: body.local_origin,
                    pending: std::collections::VecDeque::new(),
                    responses: std::collections::HashMap::new(),
                    response_chunks: std::collections::HashMap::new(),
                    active: true,
                },
            );
            sessions.changed.notify_all();
            write_json_response(stream, 200, json!({ "registered": true }))
        }
        "/preview/client/next" => {
            let body: PreviewNextRequest = parse_json_body(&request.body, "next")?;
            let public_host = normalize_public_host(&body.public_host);
            let timeout = Duration::from_secs(body.timeout_secs.clamp(1, 60));
            let started = Instant::now();
            let mut sessions_guard = sessions
                .sessions
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            loop {
                if let Some(session) = sessions_guard.get_mut(&public_host) {
                    if !session.active {
                        return write_json_response(
                            stream,
                            410,
                            json!({ "error": "session_closed" }),
                        );
                    }
                    if let Some(request) = session.pending.pop_front() {
                        return write_json_response(stream, 200, json!({ "request": request }));
                    }
                } else {
                    return write_json_response(stream, 404, json!({ "error": "missing_session" }));
                }

                let elapsed = started.elapsed();
                if elapsed >= timeout {
                    return write_json_response(stream, 200, json!({ "request": null }));
                }
                let wait_for = timeout - elapsed;
                let (guard, wait) = sessions
                    .changed
                    .wait_timeout(sessions_guard, wait_for)
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                sessions_guard = guard;
                if wait.timed_out() {
                    return write_json_response(stream, 200, json!({ "request": null }));
                }
            }
        }
        "/preview/client/respond" => {
            let body: PreviewRespondRequest = parse_json_body(&request.body, "respond")?;
            let public_host = normalize_public_host(&body.public_host);
            let mut sessions_guard = sessions
                .sessions
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let Some(session) = sessions_guard.get_mut(&public_host) else {
                return write_json_response(stream, 404, json!({ "error": "missing_session" }));
            };
            session
                .responses
                .insert(body.response.request_id.clone(), body.response);
            sessions.changed.notify_all();
            write_json_response(stream, 200, json!({ "accepted": true }))
        }
        "/preview/client/respond-chunk" => {
            let body: PreviewRespondChunkRequest = parse_json_body(&request.body, "respond-chunk")?;
            let public_host = normalize_public_host(&body.public_host);
            let mut sessions_guard = sessions
                .sessions
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let Some(session) = sessions_guard.get_mut(&public_host) else {
                return write_json_response(stream, 404, json!({ "error": "missing_session" }));
            };
            session
                .response_chunks
                .entry(body.chunk.request_id.clone())
                .or_default()
                .push_back(body.chunk);
            sessions.changed.notify_all();
            write_json_response(stream, 200, json!({ "accepted": true }))
        }
        "/preview/client/close" => {
            let body: PreviewCloseRequest = parse_json_body(&request.body, "close")?;
            let public_host = normalize_public_host(&body.public_host);
            let mut sessions_guard = sessions
                .sessions
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(session) = sessions_guard.get_mut(&public_host) {
                session.active = false;
            }
            sessions_guard.remove(&public_host);
            sessions.changed.notify_all();
            write_json_response(stream, 200, json!({ "closed": true }))
        }
        _ => write_json_response(stream, 404, json!({ "error": "not_found" })),
    }
}

#[allow(clippy::too_many_arguments)]
fn proxy_reverse_channel_request(
    stream: &mut TcpStream,
    request: IngressHttpRequest,
    request_id: String,
    host: String,
    path: String,
    started: Instant,
    sessions: Arc<PreviewClientSessions>,
    recent_failures: Arc<Mutex<Vec<PreviewIngressFailure>>>,
) -> Result<()> {
    let public_host = normalize_public_host(&host);
    let preview_request = PreviewIngressRequest {
        request_id: request_id.clone(),
        method: request.method,
        path: request.target,
        headers: request.headers.into_iter().collect::<BTreeMap<_, _>>(),
        body_base64: if request.body.is_empty() {
            None
        } else {
            Some(base64::engine::general_purpose::STANDARD.encode(request.body))
        },
    };
    let mut sessions_guard = sessions
        .sessions
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let Some(session) = sessions_guard.get_mut(&public_host) else {
        let failure = PreviewIngressFailure {
            request_id,
            host,
            path,
            status: 404,
            classification: "missing_session".to_string(),
            message: "No active Homeboy preview ingress route matches this host".to_string(),
        };
        record_failure(&recent_failures, failure.clone());
        return write_diagnostic(stream, &failure, started);
    };
    if !session.active {
        let failure = PreviewIngressFailure {
            request_id,
            host,
            path,
            status: 410,
            classification: "disconnected_session".to_string(),
            message: "Homeboy preview client session is disconnected".to_string(),
        };
        record_failure(&recent_failures, failure.clone());
        return write_diagnostic(stream, &failure, started);
    }
    let _local_origin = session.local_origin.clone();
    session.pending.push_back(preview_request);
    sessions.changed.notify_all();

    let timeout = Duration::from_secs(60);
    loop {
        if let Some(session) = sessions_guard.get_mut(&public_host) {
            if let Some(response) = session.responses.remove(&request_id) {
                drop(sessions_guard);
                if response.body_stream {
                    return write_streaming_preview_response(
                        stream,
                        response,
                        &public_host,
                        &host,
                        &path,
                        started,
                        sessions,
                        recent_failures,
                    );
                }
                return write_preview_response(stream, response, &host, &path, started);
            }
        }
        let elapsed = started.elapsed();
        if elapsed >= timeout {
            let failure = PreviewIngressFailure {
                request_id,
                host,
                path,
                status: 504,
                classification: "client_timeout".to_string(),
                message: "Homeboy preview client did not respond before timeout".to_string(),
            };
            record_failure(&recent_failures, failure.clone());
            return write_diagnostic(stream, &failure, started);
        }
        let (guard, wait) = sessions
            .changed
            .wait_timeout(sessions_guard, timeout - elapsed)
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        sessions_guard = guard;
        if wait.timed_out() {
            let failure = PreviewIngressFailure {
                request_id,
                host,
                path,
                status: 504,
                classification: "client_timeout".to_string(),
                message: "Homeboy preview client did not respond before timeout".to_string(),
            };
            record_failure(&recent_failures, failure.clone());
            return write_diagnostic(stream, &failure, started);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn proxy_request(
    stream: &mut TcpStream,
    client: &reqwest::blocking::Client,
    route: &PreviewIngressRoute,
    request: IngressHttpRequest,
    request_id: String,
    host: String,
    path: String,
    started: Instant,
    recent_failures: Arc<Mutex<Vec<PreviewIngressFailure>>>,
) -> Result<()> {
    if request.method.eq_ignore_ascii_case("OPTIONS") {
        write_status_and_headers(
            stream,
            204,
            "No Content",
            &artifact_cors_headers(Vec::new(), &path),
        )?;
        log_request(&PreviewIngressLogLine {
            request_id,
            host,
            path,
            status: 204,
            bytes: 0,
            duration_ms: started.elapsed().as_millis(),
            classification: "cors_preflight".to_string(),
        });
        return Ok(());
    }
    let upstream_url = upstream_url(route, &request.target)?;
    let method = reqwest::Method::from_bytes(request.method.as_bytes())
        .map_err(|e| Error::validation_invalid_argument("method", e.to_string(), None, None))?;
    let mut upstream = client.request(method, upstream_url);
    for (name, value) in request.headers {
        if is_hop_by_hop_header(&name) || name == "host" || name == "content-length" {
            continue;
        }
        upstream = upstream.header(&name, value);
    }
    if !request.body.is_empty() {
        upstream = upstream.body(request.body);
    }

    match upstream.send() {
        Ok(mut response) => {
            let status = response.status();
            let headers = response
                .headers()
                .iter()
                .filter_map(|(name, value)| {
                    let name = name.as_str().to_ascii_lowercase();
                    if is_hop_by_hop_header(&name) {
                        return None;
                    }
                    value.to_str().ok().map(|value| (name, value.to_string()))
                })
                .collect::<Vec<_>>();
            let headers = artifact_cors_headers(headers, &path);
            write_status_and_headers(
                stream,
                status.as_u16(),
                status.canonical_reason().unwrap_or("OK"),
                &headers,
            )?;
            let bytes = response.copy_to(stream).map_err(|e| {
                Error::internal_io(e.to_string(), Some("stream upstream response".to_string()))
            })? as usize;
            log_request(&PreviewIngressLogLine {
                request_id,
                host,
                path,
                status: status.as_u16(),
                bytes,
                duration_ms: started.elapsed().as_millis(),
                classification: "proxied".to_string(),
            });
            Ok(())
        }
        Err(error) => {
            let timeout = error.is_timeout();
            let failure = PreviewIngressFailure {
                request_id: request_id.clone(),
                host: host.clone(),
                path: path.clone(),
                status: if timeout { 504 } else { 502 },
                classification: if timeout {
                    "upstream_timeout"
                } else {
                    "upstream_error"
                }
                .to_string(),
                message: error.to_string(),
            };
            record_failure(&recent_failures, failure.clone());
            write_diagnostic(stream, &failure, started)
        }
    }
}

fn preview_token_sha256(env_name: &str) -> Option<String> {
    std::env::var(env_name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn authorized_preview_client(request: &IngressHttpRequest, auth: &PreviewIngressAuth) -> bool {
    let Some(expected) = auth.token_sha256.as_deref() else {
        eprintln!(
            "homeboy preview ingress client auth disabled: {} is not set",
            auth.token_sha256_env
        );
        return false;
    };
    let Some(token) = request.headers.iter().find_map(|(name, value)| {
        if name.eq_ignore_ascii_case("authorization") {
            value
                .strip_prefix("Bearer ")
                .or_else(|| value.strip_prefix("bearer "))
                .map(str::trim)
                .map(str::to_string)
        } else {
            None
        }
    }) else {
        return false;
    };
    let digest = Sha256::digest(token.as_bytes());
    format!("{digest:x}").eq_ignore_ascii_case(expected)
}

fn parse_json_body<T: for<'de> Deserialize<'de>>(body: &[u8], context: &str) -> Result<T> {
    serde_json::from_slice(body)
        .map_err(|e| Error::internal_json(e.to_string(), Some(context.to_string())))
}

fn validate_client_public_host(public_host: &str) -> Result<()> {
    if public_host.is_empty() || public_host.contains('*') || public_host.contains('/') {
        return Err(Error::validation_invalid_argument(
            "public_host",
            "preview client must register exactly one public host",
            Some(public_host.to_string()),
            None,
        ));
    }
    Ok(())
}

fn query_value(target: &str, key: &str) -> Option<String> {
    let query = target.split_once('?')?.1;
    for pair in query.split('&') {
        let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
        if name == key {
            return Some(value.replace('+', " "));
        }
    }
    None
}

fn validate_client_local_origin(local_origin: &str) -> Result<()> {
    let parsed = reqwest::Url::parse(local_origin).map_err(|err| {
        Error::validation_invalid_argument(
            "local_origin",
            format!("preview client local origin must be a valid HTTP(S) URL: {err}"),
            Some(local_origin.to_string()),
            None,
        )
    })?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(Error::validation_invalid_argument(
            "local_origin",
            "preview client local origin must use http or https",
            Some(local_origin.to_string()),
            None,
        ));
    }
    Ok(())
}

fn upstream_url(route: &PreviewIngressRoute, target: &str) -> Result<String> {
    let base = route.upstream_origin.trim_end_matches('/');
    let target = if target.starts_with('/') {
        target.to_string()
    } else {
        format!("/{target}")
    };
    Ok(format!("{base}{target}"))
}
