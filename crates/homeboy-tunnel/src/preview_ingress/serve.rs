use base64::Engine;
use homeboy_engine_primitives::content_hash;
use serde::Deserialize;
use serde_json::json;
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::preview_client::{
    PreviewIngressRequest, PreviewWebSocketFrame, PreviewWebSocketFrameKind, PreviewWebSocketOpen,
};
use homeboy_core::error::{Error, Result};

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
    PreviewIngressFailure, PreviewIngressLogLine, PreviewIngressRoute,
    PreviewIngressRouteLifecycle, PreviewIngressServeSpec, PreviewNextRequest,
    PreviewRegisterRequest, PreviewRespondChunkRequest, PreviewRespondRequest,
    PreviewWebSocketOperation, PreviewWebSocketSession,
};

pub const PREVIEW_WEBSOCKET_MAX_FRAME_BYTES: usize = 1024 * 1024;
pub const PREVIEW_WEBSOCKET_MAX_MESSAGE_BYTES: usize = 1024 * 1024;
pub const PREVIEW_WEBSOCKET_MAX_SESSIONS_PER_ROUTE: usize = 16;
pub const PREVIEW_WEBSOCKET_QUEUE_DEPTH: usize = 64;
pub const PREVIEW_WEBSOCKET_QUEUE_BYTES_PER_CONNECTION: usize = 4 * 1024 * 1024;
pub const PREVIEW_WEBSOCKET_QUEUE_BYTES_PER_ROUTE: usize = 16 * 1024 * 1024;
pub const PREVIEW_WEBSOCKET_IDLE_SECS: u64 = 60;
pub const PREVIEW_WEBSOCKET_HANDSHAKE_SECS: u64 = 10;
pub const PREVIEW_WEBSOCKET_CLOSE_SECS: u64 = 5;
const PREVIEW_WEBSOCKET_WRITE_SECS: u64 = 5;

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
        public_host_pattern: spec.public_host_pattern.trim().to_ascii_lowercase(),
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
    stream
        .set_read_timeout(Some(Duration::from_secs(60)))
        .map_err(|e| {
            Error::internal_io(
                e.to_string(),
                Some("configure ingress read timeout".to_string()),
            )
        })?;
    stream
        .set_write_timeout(Some(Duration::from_secs(PREVIEW_WEBSOCKET_WRITE_SECS)))
        .map_err(|e| {
            Error::internal_io(
                e.to_string(),
                Some("configure ingress write timeout".to_string()),
            )
        })?;
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

    let has_live_session = sessions
        .sessions
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(&normalize_public_host(&host))
        .is_some_and(|session| session.active);
    if has_live_session {
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
            if let Err(error) = validate_public_host_authority(&public_host, auth) {
                let failure = PreviewIngressFailure {
                    request_id: uuid::Uuid::new_v4().to_string(),
                    host: request.host.clone().unwrap_or_default(),
                    path: request.target.clone(),
                    status: 403,
                    classification: "host_claim_rejected".to_string(),
                    message: error.message.clone(),
                };
                record_failure(recent_failures, failure);
                return write_json_response(
                    stream,
                    403,
                    json!({
                        "error": "forbidden",
                        "classification": "host_claim_rejected",
                        "message": error.message,
                        "public_host": public_host,
                        "public_host_pattern": auth.public_host_pattern,
                        "hint": "A preview client may only register a public host matching the pattern this ingress was started with."
                    }),
                );
            }
            validate_client_local_origin(&body.local_origin)?;
            let session_id = body.session_id.unwrap_or_else(|| public_host.clone());
            let registered_session_id = session_id.clone();
            let channel_id = uuid::Uuid::new_v4().to_string();
            let mut sessions_guard = sessions
                .sessions
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            sessions_guard.insert(
                public_host.clone(),
                PreviewClientSession {
                    local_origin: body.local_origin,
                    session_id,
                    channel_id: channel_id.clone(),
                    pending: std::collections::VecDeque::new(),
                    pending_websockets: std::collections::VecDeque::new(),
                    responses: std::collections::HashMap::new(),
                    response_chunks: std::collections::HashMap::new(),
                    websockets: std::collections::HashMap::new(),
                    active: true,
                },
            );
            sessions.changed.notify_all();
            let registered_session_id = sessions_guard
                .get(&public_host)
                .map(|session| session.session_id.clone())
                .unwrap_or(registered_session_id);
            write_json_response(
                stream,
                200,
                json!({
                    "registered": true,
                    "channel_id": channel_id,
                    "session_id": registered_session_id,
                }),
            )
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
                    if !body.channel_id.is_empty() && body.channel_id != session.channel_id {
                        return write_json_response(
                            stream,
                            403,
                            json!({ "error": "route_owner_mismatch" }),
                        );
                    }
                    if !session.active {
                        return write_json_response(
                            stream,
                            410,
                            json!({ "error": "session_closed" }),
                        );
                    }
                    if let Some(request) = session.pending.pop_front() {
                        return write_json_response(
                            stream,
                            200,
                            json!({ "request": request, "websocket": null }),
                        );
                    }
                    if !body.channel_id.is_empty() {
                        if let Some(websocket) = session.pending_websockets.pop_front() {
                            return write_json_response(
                                stream,
                                200,
                                json!({ "request": null, "websocket": websocket }),
                            );
                        }
                    }
                } else {
                    return write_json_response(stream, 404, json!({ "error": "missing_session" }));
                }

                let elapsed = started.elapsed();
                if elapsed >= timeout {
                    return write_json_response(
                        stream,
                        200,
                        json!({ "request": null, "websocket": null }),
                    );
                }
                let wait_for = timeout - elapsed;
                let (guard, wait) = sessions
                    .changed
                    .wait_timeout(sessions_guard, wait_for)
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                sessions_guard = guard;
                if wait.timed_out() {
                    return write_json_response(
                        stream,
                        200,
                        json!({ "request": null, "websocket": null }),
                    );
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
        "/preview/client/websocket/open" => {
            let body: PreviewWebSocketOperation = parse_json_body(&request.body, "websocket open")?;
            let public_host = normalize_public_host(&body.public_host);
            let Some(result) = body.result else {
                return write_json_response(stream, 400, json!({ "error": "missing_open_result" }));
            };
            let mut guard = sessions.sessions.lock().unwrap_or_else(|p| p.into_inner());
            let Some(session) = guard.get_mut(&public_host) else {
                return write_json_response(stream, 404, json!({ "error": "missing_session" }));
            };
            if session.channel_id != body.channel_id {
                return write_json_response(
                    stream,
                    403,
                    json!({ "error": "route_owner_mismatch" }),
                );
            }
            let Some(websocket) = session.websockets.get_mut(&result.websocket_id) else {
                return write_json_response(stream, 404, json!({ "error": "missing_websocket" }));
            };
            websocket.open_result = Some(result);
            websocket.last_activity = Instant::now();
            sessions.changed.notify_all();
            write_json_response(stream, 200, json!({ "accepted": true }))
        }
        "/preview/client/websocket/next" => {
            let body: PreviewWebSocketOperation = parse_json_body(&request.body, "websocket next")?;
            let public_host = normalize_public_host(&body.public_host);
            let timeout = Duration::from_millis(body.timeout_ms.clamp(1, 1000));
            let started = Instant::now();
            let mut guard = sessions.sessions.lock().unwrap_or_else(|p| p.into_inner());
            loop {
                let Some(session) = guard.get_mut(&public_host) else {
                    return write_json_response(stream, 404, json!({ "error": "missing_session" }));
                };
                if session.channel_id != body.channel_id {
                    return write_json_response(
                        stream,
                        403,
                        json!({ "error": "route_owner_mismatch" }),
                    );
                }
                let Some(websocket) = session.websockets.get_mut(&body.websocket_id) else {
                    return write_json_response(
                        stream,
                        404,
                        json!({ "error": "missing_websocket" }),
                    );
                };
                if let Some(frame) = websocket.to_client.pop_front() {
                    websocket.to_client_bytes = websocket
                        .to_client_bytes
                        .saturating_sub(frame_payload_len(&frame));
                    if matches!(frame.kind, PreviewWebSocketFrameKind::Close) {
                        websocket.client_close_pending_delivery = true;
                    }
                    websocket.last_activity = Instant::now();
                    return write_json_response(
                        stream,
                        200,
                        json!({ "frame": frame, "closed": false }),
                    );
                }
                if websocket_close_complete(websocket) || websocket_close_expired(websocket) {
                    return write_json_response(
                        stream,
                        200,
                        json!({ "frame": null, "closed": true }),
                    );
                }
                if started.elapsed() >= timeout {
                    return write_json_response(
                        stream,
                        200,
                        json!({ "frame": null, "closed": false }),
                    );
                }
                let (next_guard, _) = sessions
                    .changed
                    .wait_timeout(guard, timeout - started.elapsed())
                    .unwrap_or_else(|p| p.into_inner());
                guard = next_guard;
            }
        }
        "/preview/client/websocket/frame" => {
            let body: PreviewWebSocketOperation =
                parse_json_body(&request.body, "websocket frame")?;
            let public_host = normalize_public_host(&body.public_host);
            let Some(frame) = body.frame else {
                return write_json_response(stream, 400, json!({ "error": "missing_frame" }));
            };
            let payload_bytes = match decoded_frame_payload_len(&frame) {
                Ok(bytes) => bytes,
                Err(error) => {
                    return write_json_response(
                        stream,
                        400,
                        json!({ "error": "invalid_websocket_payload", "message": error.message }),
                    )
                }
            };
            if payload_bytes > PREVIEW_WEBSOCKET_MAX_FRAME_BYTES {
                return write_json_response(
                    stream,
                    413,
                    json!({ "error": "websocket_frame_too_large" }),
                );
            }
            let mut guard = sessions.sessions.lock().unwrap_or_else(|p| p.into_inner());
            let Some(session) = guard.get_mut(&public_host) else {
                return write_json_response(stream, 404, json!({ "error": "missing_session" }));
            };
            if session.channel_id != body.channel_id {
                return write_json_response(
                    stream,
                    403,
                    json!({ "error": "route_owner_mismatch" }),
                );
            }
            let route_over_budget = route_queued_bytes(session, false)
                .saturating_add(payload_bytes)
                > PREVIEW_WEBSOCKET_QUEUE_BYTES_PER_ROUTE;
            let Some(websocket) = session.websockets.get_mut(&frame.websocket_id) else {
                return write_json_response(stream, 404, json!({ "error": "missing_websocket" }));
            };
            if websocket.to_public.len() >= PREVIEW_WEBSOCKET_QUEUE_DEPTH
                || websocket.to_public_bytes.saturating_add(payload_bytes)
                    > PREVIEW_WEBSOCKET_QUEUE_BYTES_PER_CONNECTION
                || route_over_budget
            {
                return write_json_response(
                    stream,
                    429,
                    json!({ "error": "websocket_queue_full" }),
                );
            }
            websocket.last_activity = Instant::now();
            if matches!(frame.kind, PreviewWebSocketFrameKind::Close) {
                websocket.client_close_received = true;
                begin_websocket_close(websocket);
            }
            websocket.to_public_bytes += payload_bytes;
            websocket.to_public.push_back(frame);
            sessions.changed.notify_all();
            write_json_response(stream, 200, json!({ "accepted": true }))
        }
        "/preview/client/websocket/abort" => {
            let body: PreviewWebSocketOperation =
                parse_json_body(&request.body, "websocket abort")?;
            let public_host = normalize_public_host(&body.public_host);
            let mut guard = sessions.sessions.lock().unwrap_or_else(|p| p.into_inner());
            let Some(session) = guard.get_mut(&public_host) else {
                return write_json_response(stream, 404, json!({ "error": "missing_session" }));
            };
            if session.channel_id != body.channel_id {
                return write_json_response(
                    stream,
                    403,
                    json!({ "error": "route_owner_mismatch" }),
                );
            }
            if session.websockets.remove(&body.websocket_id).is_none() {
                return write_json_response(stream, 404, json!({ "error": "missing_websocket" }));
            }
            session
                .pending_websockets
                .retain(|open| open.websocket_id != body.websocket_id);
            sessions.changed.notify_all();
            write_json_response(stream, 200, json!({ "aborted": true }))
        }
        "/preview/client/websocket/delivered" => {
            let body: PreviewWebSocketOperation =
                parse_json_body(&request.body, "websocket delivery acknowledgement")?;
            let public_host = normalize_public_host(&body.public_host);
            let mut guard = sessions.sessions.lock().unwrap_or_else(|p| p.into_inner());
            let Some(session) = guard.get_mut(&public_host) else {
                return write_json_response(stream, 404, json!({ "error": "missing_session" }));
            };
            if session.channel_id != body.channel_id {
                return write_json_response(
                    stream,
                    403,
                    json!({ "error": "route_owner_mismatch" }),
                );
            }
            let Some(websocket) = session.websockets.get_mut(&body.websocket_id) else {
                return write_json_response(stream, 404, json!({ "error": "missing_websocket" }));
            };
            if (websocket.client_close_sent || !websocket.client_close_received)
                && !websocket.client_close_pending_delivery
            {
                return write_json_response(
                    stream,
                    409,
                    json!({ "error": "unexpected_websocket_delivery" }),
                );
            }
            websocket.client_close_pending_delivery = false;
            websocket.client_close_sent = true;
            begin_websocket_close(websocket);
            sessions.changed.notify_all();
            write_json_response(stream, 200, json!({ "accepted": true }))
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
    if is_websocket_upgrade(&request) {
        return proxy_reverse_channel_websocket(
            stream,
            request,
            request_id,
            host,
            path,
            started,
            sessions,
            recent_failures,
        );
    }
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
fn proxy_reverse_channel_websocket(
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
    let websocket_id = uuid::Uuid::new_v4().to_string();
    let open = PreviewWebSocketOpen {
        websocket_id: websocket_id.clone(),
        path: request.target,
        headers: request.headers.clone(),
    };
    let setup_failure = {
        let mut guard = sessions.sessions.lock().unwrap_or_else(|p| p.into_inner());
        match guard.get_mut(&public_host) {
            None => Some((
                404,
                "missing_session",
                "No active Homeboy preview client is registered for this host",
            )),
            Some(session)
                if session.websockets.len() >= PREVIEW_WEBSOCKET_MAX_SESSIONS_PER_ROUTE =>
            {
                Some((
                    503,
                    "websocket_session_limit",
                    "Homeboy preview WebSocket concurrent-session limit reached",
                ))
            }
            Some(session) if session.pending_websockets.len() >= PREVIEW_WEBSOCKET_QUEUE_DEPTH => {
                Some((
                    503,
                    "websocket_queue_full",
                    "Homeboy preview WebSocket open queue is full",
                ))
            }
            Some(session) => {
                session.websockets.insert(
                    websocket_id.clone(),
                    PreviewWebSocketSession {
                        open_result: None,
                        to_client: std::collections::VecDeque::new(),
                        to_public: std::collections::VecDeque::new(),
                        to_client_bytes: 0,
                        to_public_bytes: 0,
                        last_activity: Instant::now(),
                        public_close_received: false,
                        public_close_sent: false,
                        client_close_received: false,
                        client_close_sent: false,
                        client_close_pending_delivery: false,
                        close_deadline: None,
                    },
                );
                session.pending_websockets.push_back(open);
                sessions.changed.notify_all();
                None
            }
        }
    };
    if let Some((status, classification, message)) = setup_failure {
        return websocket_diagnostic(
            stream,
            request_id,
            host,
            path,
            status,
            classification,
            message,
            started,
            &recent_failures,
        );
    }
    let cleanup = WebSocketCleanupGuard::new(
        Arc::clone(&sessions),
        public_host.clone(),
        websocket_id.clone(),
    );

    let handshake_timeout = Duration::from_secs(PREVIEW_WEBSOCKET_HANDSHAKE_SECS);
    let result = loop {
        let state = {
            let mut guard = sessions.sessions.lock().unwrap_or_else(|p| p.into_inner());
            guard
                .get_mut(&public_host)
                .and_then(|session| session.websockets.get_mut(&websocket_id))
                .map(|websocket| websocket.open_result.take())
        };
        match state {
            None => {
                return websocket_diagnostic(
                    stream,
                    request_id,
                    host,
                    path,
                    502,
                    "websocket_setup_cancelled",
                    "Homeboy preview WebSocket setup was cancelled",
                    started,
                    &recent_failures,
                )
            }
            Some(Some(result)) => break result,
            Some(None) => {}
        }
        if started.elapsed() >= handshake_timeout {
            return websocket_diagnostic(
                stream,
                request_id,
                host,
                path,
                504,
                "websocket_handshake_timeout",
                "Homeboy preview client did not complete the local WebSocket handshake",
                started,
                &recent_failures,
            );
        }
        let timed_out = {
            let guard = sessions.sessions.lock().unwrap_or_else(|p| p.into_inner());
            let (_wait_guard, wait) = sessions
                .changed
                .wait_timeout(guard, handshake_timeout - started.elapsed())
                .unwrap_or_else(|p| p.into_inner());
            wait.timed_out()
        };
        if timed_out {
            return websocket_diagnostic(
                stream,
                request_id,
                host,
                path,
                504,
                "websocket_handshake_timeout",
                "Homeboy preview client did not complete the local WebSocket handshake",
                started,
                &recent_failures,
            );
        }
    };
    if !result.accepted {
        let status = if (400..600).contains(&result.status) {
            result.status
        } else {
            502
        };
        return websocket_diagnostic(
            stream,
            request_id,
            host,
            path,
            status,
            "local_websocket_handshake_failed",
            result
                .error
                .as_deref()
                .unwrap_or("Local WebSocket origin rejected the handshake"),
            started,
            &recent_failures,
        );
    }
    let Some(key) = request.headers.iter().find_map(|(name, value)| {
        name.eq_ignore_ascii_case("sec-websocket-key")
            .then_some(value)
    }) else {
        return websocket_diagnostic(
            stream,
            request_id,
            host,
            path,
            400,
            "invalid_websocket_upgrade",
            "WebSocket upgrade is missing Sec-WebSocket-Key",
            started,
            &recent_failures,
        );
    };
    if let Err(error) = write!(
        stream,
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {}\r\n",
        tungstenite::handshake::derive_accept_key(key.as_bytes())
    ) {
        record_websocket_terminal_failure(&recent_failures, &request_id, &host, &path, "websocket_handshake_write_failed", error.to_string());
        return Err(Error::internal_io(error.to_string(), Some("write WebSocket handshake".to_string())));
    }
    for (name, value) in result.headers {
        if name.eq_ignore_ascii_case("sec-websocket-protocol") {
            if let Err(error) = write!(stream, "{name}: {value}\r\n") {
                record_websocket_terminal_failure(
                    &recent_failures,
                    &request_id,
                    &host,
                    &path,
                    "websocket_handshake_write_failed",
                    error.to_string(),
                );
                return Err(Error::internal_io(
                    error.to_string(),
                    Some("write WebSocket handshake".to_string()),
                ));
            }
        }
    }
    if let Err(error) = stream.write_all(b"\r\n") {
        record_websocket_terminal_failure(
            &recent_failures,
            &request_id,
            &host,
            &path,
            "websocket_handshake_write_failed",
            error.to_string(),
        );
        return Err(Error::internal_io(
            error.to_string(),
            Some("finish WebSocket handshake".to_string()),
        ));
    }
    stream
        .set_read_timeout(Some(Duration::from_millis(100)))
        .map_err(|e| Error::internal_io(e.to_string(), Some("configure WebSocket".to_string())))?;
    stream
        .set_write_timeout(Some(Duration::from_secs(PREVIEW_WEBSOCKET_WRITE_SECS)))
        .map_err(|e| Error::internal_io(e.to_string(), Some("configure WebSocket".to_string())))?;
    let config = tungstenite::protocol::WebSocketConfig::default()
        .max_frame_size(Some(PREVIEW_WEBSOCKET_MAX_FRAME_BYTES))
        .max_message_size(Some(PREVIEW_WEBSOCKET_MAX_MESSAGE_BYTES));
    let mut socket = tungstenite::WebSocket::from_raw_socket(
        match stream.try_clone() {
            Ok(stream) => stream,
            Err(error) => {
                record_websocket_terminal_failure(
                    &recent_failures,
                    &request_id,
                    &host,
                    &path,
                    "websocket_stream_clone_failed",
                    error.to_string(),
                );
                return Err(Error::internal_io(error.to_string(), None));
            }
        },
        tungstenite::protocol::Role::Server,
        Some(config),
    );
    let mut sequence = 0_u64;
    loop {
        let outbound = {
            let mut guard = sessions.sessions.lock().unwrap_or_else(|p| p.into_inner());
            let Some(session) = guard.get_mut(&public_host) else {
                break;
            };
            let Some(websocket) = session.websockets.get_mut(&websocket_id) else {
                break;
            };
            if websocket.last_activity.elapsed() >= Duration::from_secs(PREVIEW_WEBSOCKET_IDLE_SECS)
                && websocket.close_deadline.is_none()
            {
                let frame = PreviewWebSocketFrame {
                    websocket_id: websocket_id.clone(),
                    sequence,
                    kind: PreviewWebSocketFrameKind::Close,
                    payload_base64: String::new(),
                    close_code: Some(1001),
                    close_reason: Some("Homeboy preview WebSocket idle timeout".to_string()),
                };
                websocket.public_close_sent = true;
                websocket.to_client_bytes += frame_payload_len(&frame);
                websocket.to_client.push_back(frame.clone());
                begin_websocket_close(websocket);
                sessions.changed.notify_all();
                Some(frame)
            } else {
                let frame = websocket.to_public.pop_front();
                if let Some(frame) = frame.as_ref() {
                    websocket.to_public_bytes = websocket
                        .to_public_bytes
                        .saturating_sub(frame_payload_len(frame));
                    if matches!(frame.kind, PreviewWebSocketFrameKind::Close) {
                        websocket.public_close_sent = true;
                        begin_websocket_close(websocket);
                    }
                }
                frame
            }
        };
        if let Some(frame) = outbound {
            let closing = matches!(frame.kind, PreviewWebSocketFrameKind::Close);
            let message = match protocol_frame_to_message(frame) {
                Ok(message) => message,
                Err(error) => {
                    record_websocket_terminal_failure(
                        &recent_failures,
                        &request_id,
                        &host,
                        &path,
                        "websocket_protocol_decode_failed",
                        error.message.clone(),
                    );
                    return Err(error);
                }
            };
            if let Err(error) = socket.send(message) {
                record_websocket_terminal_failure(
                    &recent_failures,
                    &request_id,
                    &host,
                    &path,
                    "websocket_public_write_failed",
                    error.to_string(),
                );
                break;
            }
            if closing {
                continue;
            }
        }
        let closing_complete = sessions
            .sessions
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&public_host)
            .and_then(|session| session.websockets.get(&websocket_id))
            .is_none_or(|websocket| {
                websocket_close_complete(websocket) || websocket_close_expired(websocket)
            });
        if closing_complete {
            break;
        }
        match socket.read() {
            Ok(message) => {
                let frame = message_to_protocol_frame(&websocket_id, sequence, message);
                sequence += 1;
                let closing = matches!(frame.kind, PreviewWebSocketFrameKind::Close);
                let bytes = frame_payload_len(&frame);
                let mut guard = sessions.sessions.lock().unwrap_or_else(|p| p.into_inner());
                let route_over_budget = guard.get(&public_host).is_some_and(|session| {
                    route_queued_bytes(session, true).saturating_add(bytes)
                        > PREVIEW_WEBSOCKET_QUEUE_BYTES_PER_ROUTE
                });
                let Some(websocket) = guard
                    .get_mut(&public_host)
                    .and_then(|session| session.websockets.get_mut(&websocket_id))
                else {
                    break;
                };
                if bytes > PREVIEW_WEBSOCKET_MAX_FRAME_BYTES
                    || websocket.to_client.len() >= PREVIEW_WEBSOCKET_QUEUE_DEPTH
                    || websocket.to_client_bytes.saturating_add(bytes)
                        > PREVIEW_WEBSOCKET_QUEUE_BYTES_PER_CONNECTION
                    || route_over_budget
                {
                    record_websocket_terminal_failure(
                        &recent_failures,
                        &request_id,
                        &host,
                        &path,
                        "websocket_public_queue_full",
                        "WebSocket public-to-client queue limit exceeded".to_string(),
                    );
                    break;
                }
                websocket.to_client_bytes += bytes;
                websocket.last_activity = Instant::now();
                websocket.to_client.push_back(frame);
                sessions.changed.notify_all();
                if closing {
                    websocket.public_close_received = true;
                    begin_websocket_close(websocket);
                    continue;
                }
            }
            Err(tungstenite::Error::Io(error))
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed) => {
                let closing = sessions
                    .sessions
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .get(&public_host)
                    .and_then(|session| session.websockets.get(&websocket_id))
                    .is_some_and(|websocket| websocket.close_deadline.is_some());
                if closing {
                    thread::sleep(Duration::from_millis(10));
                    continue;
                }
                break;
            }
            Err(error) => {
                record_websocket_terminal_failure(
                    &recent_failures,
                    &request_id,
                    &host,
                    &path,
                    "websocket_public_read_failed",
                    error.to_string(),
                );
                break;
            }
        }
    }
    drop(cleanup);
    Ok(())
}

fn is_websocket_upgrade(request: &IngressHttpRequest) -> bool {
    request.method.eq_ignore_ascii_case("GET")
        && request.headers.iter().any(|(name, value)| {
            name.eq_ignore_ascii_case("upgrade") && value.eq_ignore_ascii_case("websocket")
        })
        && request.headers.iter().any(|(name, value)| {
            name.eq_ignore_ascii_case("connection")
                && value
                    .split(',')
                    .any(|part| part.trim().eq_ignore_ascii_case("upgrade"))
        })
}

fn frame_payload_len(frame: &PreviewWebSocketFrame) -> usize {
    decoded_frame_payload_len(frame).unwrap_or(0)
}

fn decoded_frame_payload_len(frame: &PreviewWebSocketFrame) -> Result<usize> {
    let payload = base64::engine::general_purpose::STANDARD
        .decode(&frame.payload_base64)
        .map_err(|error| {
            Error::validation_invalid_argument("payload_base64", error.to_string(), None, None)
        })?;
    Ok(payload.len() + frame.close_reason.as_ref().map_or(0, String::len))
}

fn route_queued_bytes(session: &PreviewClientSession, to_client: bool) -> usize {
    session
        .websockets
        .values()
        .map(|websocket| {
            if to_client {
                websocket.to_client_bytes
            } else {
                websocket.to_public_bytes
            }
        })
        .sum()
}

fn begin_websocket_close(websocket: &mut PreviewWebSocketSession) {
    websocket
        .close_deadline
        .get_or_insert_with(|| Instant::now() + Duration::from_secs(PREVIEW_WEBSOCKET_CLOSE_SECS));
}

fn websocket_close_complete(websocket: &PreviewWebSocketSession) -> bool {
    websocket.public_close_received
        && websocket.public_close_sent
        && websocket.client_close_received
        && websocket.client_close_sent
}

fn websocket_close_expired(websocket: &PreviewWebSocketSession) -> bool {
    websocket
        .close_deadline
        .is_some_and(|deadline| Instant::now() >= deadline)
}

struct WebSocketCleanupGuard {
    sessions: Arc<PreviewClientSessions>,
    public_host: String,
    websocket_id: String,
}

impl WebSocketCleanupGuard {
    fn new(
        sessions: Arc<PreviewClientSessions>,
        public_host: String,
        websocket_id: String,
    ) -> Self {
        Self {
            sessions,
            public_host,
            websocket_id,
        }
    }
}

impl Drop for WebSocketCleanupGuard {
    fn drop(&mut self) {
        cleanup_websocket(&self.sessions, &self.public_host, &self.websocket_id);
    }
}

fn record_websocket_terminal_failure(
    recent_failures: &Arc<Mutex<Vec<PreviewIngressFailure>>>,
    request_id: &str,
    host: &str,
    path: &str,
    classification: &str,
    message: String,
) {
    record_failure(
        recent_failures,
        PreviewIngressFailure {
            request_id: request_id.to_string(),
            host: host.to_string(),
            path: path.to_string(),
            status: 502,
            classification: classification.to_string(),
            message,
        },
    );
}

fn message_to_protocol_frame(
    websocket_id: &str,
    sequence: u64,
    message: tungstenite::Message,
) -> PreviewWebSocketFrame {
    let (kind, payload, close_code, close_reason) = match message {
        tungstenite::Message::Text(value) => (
            PreviewWebSocketFrameKind::Text,
            value.as_bytes().to_vec(),
            None,
            None,
        ),
        tungstenite::Message::Binary(value) => (
            PreviewWebSocketFrameKind::Binary,
            value.to_vec(),
            None,
            None,
        ),
        tungstenite::Message::Ping(value) => {
            (PreviewWebSocketFrameKind::Ping, value.to_vec(), None, None)
        }
        tungstenite::Message::Pong(value) => {
            (PreviewWebSocketFrameKind::Pong, value.to_vec(), None, None)
        }
        tungstenite::Message::Close(frame) => (
            PreviewWebSocketFrameKind::Close,
            Vec::new(),
            frame.as_ref().map(|frame| u16::from(frame.code)),
            frame.map(|frame| frame.reason.to_string()),
        ),
        tungstenite::Message::Frame(_) => {
            (PreviewWebSocketFrameKind::Binary, Vec::new(), None, None)
        }
    };
    PreviewWebSocketFrame {
        websocket_id: websocket_id.to_string(),
        sequence,
        kind,
        payload_base64: base64::engine::general_purpose::STANDARD.encode(payload),
        close_code,
        close_reason,
    }
}

fn protocol_frame_to_message(frame: PreviewWebSocketFrame) -> Result<tungstenite::Message> {
    let payload = base64::engine::general_purpose::STANDARD
        .decode(&frame.payload_base64)
        .map_err(|e| {
            Error::validation_invalid_argument("payload_base64", e.to_string(), None, None)
        })?;
    Ok(match frame.kind {
        PreviewWebSocketFrameKind::Text => tungstenite::Message::Text(
            String::from_utf8(payload)
                .map_err(|e| {
                    Error::validation_invalid_argument("websocket_text", e.to_string(), None, None)
                })?
                .into(),
        ),
        PreviewWebSocketFrameKind::Binary => tungstenite::Message::Binary(payload.into()),
        PreviewWebSocketFrameKind::Ping => tungstenite::Message::Ping(payload.into()),
        PreviewWebSocketFrameKind::Pong => tungstenite::Message::Pong(payload.into()),
        PreviewWebSocketFrameKind::Close => {
            tungstenite::Message::Close(Some(tungstenite::protocol::CloseFrame {
                code: frame.close_code.unwrap_or(1000).into(),
                reason: frame.close_reason.unwrap_or_default().into(),
            }))
        }
    })
}

fn cleanup_websocket(sessions: &PreviewClientSessions, public_host: &str, websocket_id: &str) {
    let mut guard = sessions.sessions.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(session) = guard.get_mut(public_host) {
        session.websockets.remove(websocket_id);
        session
            .pending_websockets
            .retain(|open| open.websocket_id != websocket_id);
    }
    sessions.changed.notify_all();
}

#[allow(clippy::too_many_arguments)]
fn websocket_diagnostic(
    stream: &mut TcpStream,
    request_id: String,
    host: String,
    path: String,
    status: u16,
    classification: &str,
    message: &str,
    started: Instant,
    recent_failures: &Arc<Mutex<Vec<PreviewIngressFailure>>>,
) -> Result<()> {
    let failure = PreviewIngressFailure {
        request_id,
        host,
        path,
        status,
        classification: classification.to_string(),
        message: message.to_string(),
    };
    record_failure(recent_failures, failure.clone());
    write_diagnostic(stream, &failure, started)
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
    // Fail CLOSED: with no configured digest there is nothing to authenticate
    // against, so every client request is denied rather than admitted.
    let Some(expected) = auth.token_sha256.as_deref() else {
        eprintln!(
            "homeboy preview ingress denying all client requests: {} is not set, so no client token can be verified",
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
    // Deliberately case-insensitive: `expected` is operator-supplied config
    // and is not guaranteed to be lowercase.
    content_hash::sha256_hex(token.as_bytes()).eq_ignore_ascii_case(expected)
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
    // A registered host becomes a routing key matched against inbound Host
    // headers, so restrict it to the DNS hostname charset. Without this, a
    // claim like `attacker.example#x-tunnel.operator.example` still satisfies a
    // `*-tunnel.operator.example` pattern while never being a host real traffic
    // can arrive on.
    if !public_host
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
    {
        return Err(Error::validation_invalid_argument(
            "public_host",
            "preview client public host must contain only DNS hostname characters",
            Some(public_host.to_string()),
            None,
        ));
    }
    Ok(())
}

/// Bind a claimed public host to the host authority this ingress was started with.
///
/// The shared bearer token is a single secret held by every client of an
/// ingress, so on its own it cannot distinguish one client from another. Session
/// state is keyed by public host, and registration overwrites unconditionally,
/// so an unbound `public_host` lets any token holder claim any host -- including
/// one already owned by another client -- and intercept its traffic.
///
/// `public_host_pattern` is the same value the operator puts in the reverse
/// proxy `server_name`, so a host outside it can never legitimately reach this
/// ingress anyway. Enforcing it here closes the takeover path without
/// constraining any host that real traffic can arrive on.
fn validate_public_host_authority(public_host: &str, auth: &PreviewIngressAuth) -> Result<()> {
    let pattern = auth.public_host_pattern.trim();
    if pattern.is_empty() {
        return Err(Error::validation_invalid_argument(
            "public_host",
            "preview ingress has no public host pattern configured, so no host claim can be authorized",
            Some(public_host.to_string()),
            Some(vec![
                "restart the ingress with --public-host-pattern '*-tunnel.<domain>'".to_string(),
            ]),
        ));
    }
    if pattern == public_host || glob_match::glob_match(pattern, public_host) {
        return Ok(());
    }
    Err(Error::validation_invalid_argument(
        "public_host",
        "preview client is not authorized to claim a public host outside this ingress's host pattern",
        Some(public_host.to_string()),
        Some(vec![pattern.to_string()]),
    ))
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

#[cfg(test)]
mod websocket_state_tests {
    use super::*;

    fn websocket() -> PreviewWebSocketSession {
        PreviewWebSocketSession {
            open_result: None,
            to_client: std::collections::VecDeque::new(),
            to_public: std::collections::VecDeque::new(),
            to_client_bytes: 0,
            to_public_bytes: 0,
            last_activity: Instant::now(),
            public_close_received: false,
            public_close_sent: false,
            client_close_received: false,
            client_close_sent: false,
            client_close_pending_delivery: false,
            close_deadline: None,
        }
    }

    fn session() -> PreviewClientSession {
        PreviewClientSession {
            local_origin: "http://localhost".to_string(),
            session_id: "test".to_string(),
            channel_id: "channel".to_string(),
            pending: std::collections::VecDeque::new(),
            pending_websockets: std::collections::VecDeque::new(),
            responses: std::collections::HashMap::new(),
            response_chunks: std::collections::HashMap::new(),
            websockets: std::collections::HashMap::new(),
            active: true,
        }
    }

    #[test]
    fn cleanup_guard_releases_slot_after_setup_error() {
        let sessions = Arc::new(PreviewClientSessions::default());
        sessions
            .sessions
            .lock()
            .expect("sessions lock")
            .insert("host".to_string(), session());
        sessions
            .sessions
            .lock()
            .expect("sessions lock")
            .get_mut("host")
            .expect("session")
            .websockets
            .insert("socket".to_string(), websocket());
        let cleanup = WebSocketCleanupGuard::new(
            Arc::clone(&sessions),
            "host".to_string(),
            "socket".to_string(),
        );
        drop(cleanup);
        assert!(sessions
            .sessions
            .lock()
            .expect("sessions lock")
            .get("host")
            .expect("session")
            .websockets
            .is_empty());
    }

    #[test]
    fn close_state_waits_for_delayed_peer_acknowledgements() {
        let mut state = websocket();
        state.public_close_received = true;
        state.client_close_sent = true;
        begin_websocket_close(&mut state);
        assert!(!websocket_close_complete(&state));
        state.client_close_received = true;
        state.public_close_sent = true;
        assert!(websocket_close_complete(&state));
    }

    #[test]
    fn route_byte_budget_sums_connections_in_each_direction() {
        let mut state = session();
        let mut first = websocket();
        first.to_client_bytes = PREVIEW_WEBSOCKET_QUEUE_BYTES_PER_ROUTE / 2;
        let mut second = websocket();
        second.to_client_bytes = PREVIEW_WEBSOCKET_QUEUE_BYTES_PER_ROUTE / 2;
        state.websockets.insert("first".to_string(), first);
        state.websockets.insert("second".to_string(), second);
        assert_eq!(
            route_queued_bytes(&state, true),
            PREVIEW_WEBSOCKET_QUEUE_BYTES_PER_ROUTE
        );
        assert!(
            route_queued_bytes(&state, true).saturating_add(1)
                > PREVIEW_WEBSOCKET_QUEUE_BYTES_PER_ROUTE
        );
    }

    #[test]
    fn connection_byte_budget_rejects_a_frame_after_exact_budget() {
        let mut state = websocket();
        state.to_public_bytes = PREVIEW_WEBSOCKET_QUEUE_BYTES_PER_CONNECTION;
        assert!(
            state.to_public_bytes.saturating_add(1) > PREVIEW_WEBSOCKET_QUEUE_BYTES_PER_CONNECTION
        );
        state.to_client_bytes = PREVIEW_WEBSOCKET_QUEUE_BYTES_PER_CONNECTION;
        assert!(
            state.to_client_bytes.saturating_add(1) > PREVIEW_WEBSOCKET_QUEUE_BYTES_PER_CONNECTION
        );
    }
}
