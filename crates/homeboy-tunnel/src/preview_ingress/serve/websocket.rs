use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) fn proxy_reverse_channel_websocket(
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

pub(super) fn is_websocket_upgrade(request: &IngressHttpRequest) -> bool {
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

pub(super) fn frame_payload_len(frame: &PreviewWebSocketFrame) -> usize {
    decoded_frame_payload_len(frame).unwrap_or(0)
}

pub(super) fn decoded_frame_payload_len(frame: &PreviewWebSocketFrame) -> Result<usize> {
    let payload = base64::engine::general_purpose::STANDARD
        .decode(&frame.payload_base64)
        .map_err(|error| {
            Error::validation_invalid_argument("payload_base64", error.to_string(), None, None)
        })?;
    Ok(payload.len() + frame.close_reason.as_ref().map_or(0, String::len))
}

pub(super) fn route_queued_bytes(session: &PreviewClientSession, to_client: bool) -> usize {
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

pub(super) fn begin_websocket_close(websocket: &mut PreviewWebSocketSession) {
    websocket
        .close_deadline
        .get_or_insert_with(|| Instant::now() + Duration::from_secs(PREVIEW_WEBSOCKET_CLOSE_SECS));
}

pub(super) fn websocket_close_complete(websocket: &PreviewWebSocketSession) -> bool {
    websocket.public_close_received
        && websocket.public_close_sent
        && websocket.client_close_received
        && websocket.client_close_sent
}

pub(super) fn websocket_close_expired(websocket: &PreviewWebSocketSession) -> bool {
    websocket
        .close_deadline
        .is_some_and(|deadline| Instant::now() >= deadline)
}

pub(super) struct WebSocketCleanupGuard {
    sessions: Arc<PreviewClientSessions>,
    public_host: String,
    websocket_id: String,
}

impl WebSocketCleanupGuard {
    pub(super) fn new(
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
