use base64::Engine;
use std::io::Write;
use std::net::TcpStream;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::core::error::{Error, Result};
use crate::core::preview_client::PreviewIngressResponse;

use super::types::{PreviewClientSessions, PreviewIngressFailure, PreviewIngressLogLine};

pub(crate) fn write_json_response(
    stream: &mut TcpStream,
    status: u16,
    body: serde_json::Value,
) -> Result<()> {
    let body = serde_json::to_vec(&body).map_err(|e| {
        Error::internal_json(e.to_string(), Some("preview ingress json".to_string()))
    })?;
    write_response(
        stream,
        status,
        reason_for_status(status),
        &[(&"content-type".to_string(), "application/json".to_string())],
        &body,
    )
}

pub(crate) fn write_preview_response(
    stream: &mut TcpStream,
    response: PreviewIngressResponse,
    host: &str,
    path: &str,
    started: Instant,
) -> Result<()> {
    let body = base64::engine::general_purpose::STANDARD
        .decode(response.body_base64.as_bytes())
        .map_err(|e| Error::internal_json(e.to_string(), Some(response.request_id.clone())))?;
    let mut headers = response.headers;
    push_header_if_missing(&mut headers, "content-length", &body.len().to_string());
    write_status_and_headers(
        stream,
        response.status,
        reason_for_status(response.status),
        &headers,
    )?;
    stream.write_all(&body).map_err(|e| {
        Error::internal_io(
            e.to_string(),
            Some("write preview client response body".to_string()),
        )
    })?;
    log_request(&PreviewIngressLogLine {
        request_id: response.request_id,
        host: host.to_string(),
        path: path.to_string(),
        status: response.status,
        bytes: body.len(),
        duration_ms: started.elapsed().as_millis(),
        classification: "reverse_channel".to_string(),
    });
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn write_streaming_preview_response(
    stream: &mut TcpStream,
    response: PreviewIngressResponse,
    public_host: &str,
    host: &str,
    path: &str,
    started: Instant,
    sessions: Arc<PreviewClientSessions>,
    recent_failures: Arc<Mutex<Vec<PreviewIngressFailure>>>,
) -> Result<()> {
    let request_id = response.request_id.clone();
    let status = response.status;
    let headers = response.headers.into_iter().collect::<Vec<_>>();
    write_status_and_headers(stream, status, reason_for_status(status), &headers)?;

    let idle_timeout = std::time::Duration::from_secs(60);
    let mut bytes = 0_usize;
    loop {
        let started_waiting = Instant::now();
        let mut sessions_guard = sessions
            .sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        loop {
            let Some(session) = sessions_guard.get_mut(public_host) else {
                let failure = PreviewIngressFailure {
                    request_id: request_id.clone(),
                    host: host.to_string(),
                    path: path.to_string(),
                    status: 410,
                    classification: "disconnected_session".to_string(),
                    message: "Homeboy preview client session disconnected while streaming"
                        .to_string(),
                };
                record_failure(&recent_failures, failure);
                return Ok(());
            };
            if let Some(queue) = session.response_chunks.get_mut(&request_id) {
                if let Some(chunk) = queue.pop_front() {
                    if queue.is_empty() && chunk.complete {
                        session.response_chunks.remove(&request_id);
                    }
                    drop(sessions_guard);
                    let body = base64::engine::general_purpose::STANDARD
                        .decode(chunk.body_base64.as_bytes())
                        .map_err(|e| {
                            Error::internal_json(e.to_string(), Some(request_id.clone()))
                        })?;
                    if !body.is_empty() {
                        stream.write_all(&body).map_err(|e| {
                            Error::internal_io(
                                e.to_string(),
                                Some("write preview client response chunk".to_string()),
                            )
                        })?;
                        bytes += body.len();
                    }
                    if chunk.complete {
                        log_request(&PreviewIngressLogLine {
                            request_id,
                            host: host.to_string(),
                            path: path.to_string(),
                            status,
                            bytes,
                            duration_ms: started.elapsed().as_millis(),
                            classification: "reverse_channel_stream".to_string(),
                        });
                        return Ok(());
                    }
                    break;
                }
            }
            let elapsed = started_waiting.elapsed();
            if elapsed >= idle_timeout {
                let failure = PreviewIngressFailure {
                    request_id: request_id.clone(),
                    host: host.to_string(),
                    path: path.to_string(),
                    status: 504,
                    classification: "client_stream_timeout".to_string(),
                    message: "Homeboy preview client stopped sending response chunks".to_string(),
                };
                record_failure(&recent_failures, failure);
                return Ok(());
            }
            let (guard, wait) = sessions
                .changed
                .wait_timeout(sessions_guard, idle_timeout - elapsed)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            sessions_guard = guard;
            if wait.timed_out() {
                let failure = PreviewIngressFailure {
                    request_id: request_id.clone(),
                    host: host.to_string(),
                    path: path.to_string(),
                    status: 504,
                    classification: "client_stream_timeout".to_string(),
                    message: "Homeboy preview client stopped sending response chunks".to_string(),
                };
                record_failure(&recent_failures, failure);
                return Ok(());
            }
        }
    }
}

pub(crate) fn write_diagnostic(
    stream: &mut TcpStream,
    failure: &PreviewIngressFailure,
    started: Instant,
) -> Result<()> {
    let body = serde_json::to_vec_pretty(failure).map_err(|e| {
        Error::internal_json(
            e.to_string(),
            Some("preview ingress diagnostic".to_string()),
        )
    })?;
    write_response(
        stream,
        failure.status,
        reason_for_status(failure.status),
        &[(&"content-type".to_string(), "application/json".to_string())],
        &body,
    )?;
    log_request(&PreviewIngressLogLine {
        request_id: failure.request_id.clone(),
        host: failure.host.clone(),
        path: failure.path.clone(),
        status: failure.status,
        bytes: body.len(),
        duration_ms: started.elapsed().as_millis(),
        classification: failure.classification.clone(),
    });
    Ok(())
}

pub(crate) fn write_response(
    stream: &mut TcpStream,
    status: u16,
    reason: &str,
    headers: &[(&String, String)],
    body: &[u8],
) -> Result<()> {
    let mut owned_headers = headers
        .iter()
        .map(|(name, value)| ((*name).clone(), value.clone()))
        .collect::<Vec<_>>();
    push_header_if_missing(
        &mut owned_headers,
        "content-length",
        &body.len().to_string(),
    );
    write_status_and_headers(stream, status, reason, &owned_headers)?;
    stream
        .write_all(body)
        .map_err(|e| Error::internal_io(e.to_string(), Some("write response body".to_string())))
}

pub(crate) fn write_status_and_headers(
    stream: &mut TcpStream,
    status: u16,
    reason: &str,
    headers: &[(String, String)],
) -> Result<()> {
    write!(stream, "HTTP/1.1 {} {}\r\n", status, reason)
        .map_err(|e| Error::internal_io(e.to_string(), Some("write status".to_string())))?;
    let has_connection = headers.iter().any(|(name, _)| name == "connection");
    for (name, value) in headers {
        write!(stream, "{}: {}\r\n", name, value)
            .map_err(|e| Error::internal_io(e.to_string(), Some("write header".to_string())))?;
    }
    if !has_connection {
        write!(stream, "connection: close\r\n").map_err(|e| {
            Error::internal_io(e.to_string(), Some("write connection header".to_string()))
        })?;
    }
    write!(stream, "\r\n")
        .map_err(|e| Error::internal_io(e.to_string(), Some("write header terminator".to_string())))
}

pub(crate) fn artifact_cors_headers(
    mut headers: Vec<(String, String)>,
    path: &str,
) -> Vec<(String, String)> {
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

pub(crate) fn reason_for_status(status: u16) -> &'static str {
    match status {
        204 => "No Content",
        302 => "Found",
        404 => "Not Found",
        410 => "Gone",
        502 => "Bad Gateway",
        504 => "Gateway Timeout",
        _ => "OK",
    }
}

pub(crate) fn is_hop_by_hop_header(name: &str) -> bool {
    matches!(
        name,
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
    )
}

pub(crate) fn record_failure(
    recent_failures: &Arc<Mutex<Vec<PreviewIngressFailure>>>,
    failure: PreviewIngressFailure,
) {
    let mut failures = recent_failures
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    failures.push(failure);
    if failures.len() > 50 {
        failures.remove(0);
    }
}

pub(crate) fn log_request(line: &PreviewIngressLogLine) {
    match serde_json::to_string(line) {
        Ok(line) => eprintln!("{}", line),
        Err(_) => eprintln!("preview ingress request log serialization failed"),
    }
}
