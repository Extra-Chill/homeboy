use std::io::{Read, Write};
use std::sync::Arc;

use serde_json::Value;

use crate::core::api_jobs::{JobEventKind, JobStore, RemoteRunnerJobResult};

pub(super) fn spawn_mock_broker(
    store: JobStore,
    expected_requests: usize,
) -> (String, std::thread::JoinHandle<()>) {
    spawn_mock_broker_with_paths(store, expected_requests, None)
}

pub(super) fn spawn_mock_broker_with_paths(
    store: JobStore,
    expected_requests: usize,
    seen_paths: Option<Arc<std::sync::Mutex<Vec<String>>>>,
) -> (String, std::thread::JoinHandle<()>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("listener");
    let addr = listener.local_addr().expect("addr");
    let handle = std::thread::spawn(move || {
        for _ in 0..expected_requests {
            let (mut stream, _) = listener.accept().expect("accept request");
            let request = read_request(&mut stream);
            if let Some(seen_paths) = &seen_paths {
                seen_paths
                    .lock()
                    .expect("record request path")
                    .push(request.path.clone());
            }
            let response = handle_request(&store, &request);
            write_response(&mut stream, response);
        }
    });
    (format!("http://{addr}"), handle)
}

pub(super) fn spawn_cancelling_after_claim_broker(
    store: JobStore,
    expected_requests: usize,
    seen_paths: Option<Arc<std::sync::Mutex<Vec<String>>>>,
) -> (String, std::thread::JoinHandle<()>) {
    spawn_custom_broker(store, expected_requests, seen_paths, |store, request| {
        if request.path == "/runner/jobs/claim" {
            let claim = store
                .claim_remote_runner_job("lab", None, 30_000, None)
                .expect("claim job");
            if let Some(claim) = &claim {
                store
                    .cancel(claim.job.id, "user requested")
                    .expect("cancel job");
            }
            return serde_json::json!({
                "success": true,
                "data": { "body": { "claim": claim } }
            });
        }
        handle_request(store, request)
    })
}

pub(super) fn spawn_cancelling_on_second_snapshot_broker(
    store: JobStore,
    expected_requests: usize,
    seen_paths: Option<Arc<std::sync::Mutex<Vec<String>>>>,
) -> (String, std::thread::JoinHandle<()>) {
    let snapshots = Arc::new(std::sync::Mutex::new(0_u8));
    spawn_custom_broker(
        store,
        expected_requests,
        seen_paths,
        move |store, request| {
            if let Some(job_id) = request.path.strip_prefix("/jobs/") {
                let job_id = uuid::Uuid::parse_str(job_id).expect("job id");
                let mut snapshots = snapshots.lock().expect("snapshot count");
                *snapshots += 1;
                if *snapshots == 2 {
                    store.cancel(job_id, "user requested").expect("cancel job");
                }
                let job = store.get(job_id).expect("job");
                return serde_json::json!({
                    "success": true,
                    "data": { "body": { "job": job } }
                });
            }
            handle_request(store, request)
        },
    )
}

fn spawn_custom_broker<F>(
    store: JobStore,
    expected_requests: usize,
    seen_paths: Option<Arc<std::sync::Mutex<Vec<String>>>>,
    mut handle: F,
) -> (String, std::thread::JoinHandle<()>)
where
    F: FnMut(&JobStore, &MockRequest) -> Value + Send + 'static,
{
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("listener");
    let addr = listener.local_addr().expect("addr");
    let handle = std::thread::spawn(move || {
        for _ in 0..expected_requests {
            let (mut stream, _) = listener.accept().expect("accept request");
            let request = read_request(&mut stream);
            if let Some(seen_paths) = &seen_paths {
                seen_paths
                    .lock()
                    .expect("record request path")
                    .push(request.path.clone());
            }
            let response = handle(&store, &request);
            write_response(&mut stream, response);
        }
    });
    (format!("http://{addr}"), handle)
}

pub(super) fn write_reverse_controller_session(broker_url: &str) {
    let path = crate::core::paths::runner_session_file("lab").expect("session path");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create session dir");
    }
    let session = crate::core::runner::RunnerSession {
        runner_id: "lab".to_string(),
        mode: crate::core::runner::RunnerTunnelMode::Reverse,
        role: crate::core::runner::RunnerSessionRole::Controller,
        server_id: None,
        controller_id: Some("controller".to_string()),
        broker_url: Some(broker_url.to_string()),
        remote_daemon_address: None,
        local_port: None,
        local_url: None,
        tunnel_pid: None,
        remote_daemon_pid: None,
        homeboy_version: "test".to_string(),
        homeboy_build_identity: None,
        connected_at: "2026-06-19T00:00:00Z".to_string(),
        worker_identity: None,
        worker_pid: None,
        last_seen_at: None,
    };
    std::fs::write(
        path,
        serde_json::to_string(&session).expect("serialize session"),
    )
    .expect("write session");
}

struct MockRequest {
    method: String,
    path: String,
    body: Value,
}

fn read_request(stream: &mut std::net::TcpStream) -> MockRequest {
    let mut buffer = Vec::new();
    let mut temp = [0_u8; 1024];
    let header_end = loop {
        let read = stream.read(&mut temp).expect("read request");
        assert_ne!(read, 0, "request closed before headers");
        buffer.extend_from_slice(&temp[..read]);
        if let Some(index) = find_header_end(&buffer) {
            break index;
        }
    };
    let headers = String::from_utf8_lossy(&buffer[..header_end]).to_string();
    let mut request_line = headers
        .lines()
        .next()
        .expect("request line")
        .split_whitespace();
    let method = request_line.next().expect("request method").to_string();
    let path = request_line.next().expect("request path").to_string();
    let content_length = headers
        .lines()
        .find_map(|line| line.strip_prefix("content-length: "))
        .or_else(|| {
            headers
                .lines()
                .find_map(|line| line.strip_prefix("Content-Length: "))
        })
        .and_then(|value| value.trim().parse::<usize>().ok())
        .unwrap_or(0);
    let body_start = header_end + 4;
    while buffer.len() < body_start + content_length {
        let read = stream.read(&mut temp).expect("read body");
        assert_ne!(read, 0, "request closed before body");
        buffer.extend_from_slice(&temp[..read]);
    }
    let body = if content_length == 0 {
        Value::Null
    } else {
        serde_json::from_slice(&buffer[body_start..body_start + content_length])
            .expect("request json")
    };
    MockRequest { method, path, body }
}

fn find_header_end(buffer: &[u8]) -> Option<usize> {
    buffer.windows(4).position(|window| window == b"\r\n\r\n")
}

fn handle_request(store: &JobStore, request: &MockRequest) -> Value {
    if request.method == "GET" {
        if let Some(job_id) = request.path.strip_prefix("/jobs/") {
            let job_id = uuid::Uuid::parse_str(job_id).expect("job id");
            let job = store.get(job_id).expect("job");
            return serde_json::json!({
                "success": true,
                "data": { "body": { "job": job } }
            });
        }
    }
    if request.path == "/runner/jobs/claim" {
        let claim = store
            .claim_remote_runner_job("lab", None, 30_000, None)
            .expect("claim job");
        return serde_json::json!({
            "success": true,
            "data": { "body": { "claim": claim } }
        });
    }
    if let Some(job_id) = request
        .path
        .strip_prefix("/runner/jobs/")
        .and_then(|tail| tail.strip_suffix("/events"))
    {
        let job_id = uuid::Uuid::parse_str(job_id).expect("event job id");
        let event = store
            .append_remote_runner_event(
                job_id,
                "lab",
                request.body["claim_id"].as_str().expect("event claim id"),
                JobEventKind::Progress,
                request.body["message"].as_str().map(ToString::to_string),
                None,
            )
            .expect("append event");
        return serde_json::json!({
            "success": true,
            "data": { "body": { "event": event } }
        });
    }
    if let Some(job_id) = request
        .path
        .strip_prefix("/runner/jobs/")
        .and_then(|tail| tail.strip_suffix("/finish"))
    {
        let job_id = uuid::Uuid::parse_str(job_id).expect("finish job id");
        let result: RemoteRunnerJobResult =
            serde_json::from_value(request.body["result"].clone()).expect("finish result");
        let job = store
            .finish_remote_runner_job(
                job_id,
                "lab",
                request.body["claim_id"].as_str().expect("finish claim id"),
                result,
            )
            .expect("finish job");
        return serde_json::json!({
            "success": true,
            "data": { "body": { "job": job } }
        });
    }
    serde_json::json!({
        "success": false,
        "error": { "message": "unknown mock path" }
    })
}

fn write_response(stream: &mut std::net::TcpStream, body: Value) {
    let body = body.to_string();
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    stream
        .write_all(response.as_bytes())
        .expect("write response");
}

pub(super) fn spawn_failing_broker(
    expected_requests: usize,
) -> (String, std::thread::JoinHandle<()>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("listener");
    let addr = listener.local_addr().expect("addr");
    let handle = std::thread::spawn(move || {
        for _ in 0..expected_requests {
            let (mut stream, _) = listener.accept().expect("accept request");
            let _ = read_request(&mut stream);
            write_response(
                &mut stream,
                serde_json::json!({
                    "success": false,
                    "error": { "message": "broker unavailable" }
                }),
            );
        }
    });
    (format!("http://{addr}"), handle)
}
