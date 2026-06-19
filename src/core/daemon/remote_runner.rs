use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{daemon_endpoint_response, error_response, HttpResponse};
use crate::core::api_jobs::{
    JobEventKind, JobStore, RemoteRunnerJobRequest, RemoteRunnerJobResult,
};
use crate::core::error::{Error, Result};
use crate::core::paths;
use crate::core::runner::{self, RunnerSession, RunnerSessionRole, RunnerTunnelMode};

#[derive(Debug, Clone, Deserialize)]
struct ClaimRequest {
    runner_id: String,
    #[serde(default)]
    project_id: Option<String>,
    #[serde(default)]
    lease_ms: Option<u64>,
    #[serde(default)]
    concurrency_limit: Option<usize>,
}

#[derive(Debug, Clone, Deserialize)]
struct EventRequest {
    runner_id: String,
    claim_id: String,
    kind: JobEventKind,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    data: Option<Value>,
}

#[derive(Debug, Clone, Deserialize)]
struct FinishRequest {
    runner_id: String,
    claim_id: String,
    result: RemoteRunnerJobResult,
}

#[derive(Debug, Clone, Deserialize)]
struct HeartbeatRequest {
    runner_id: String,
    claim_id: String,
    #[serde(default)]
    lease_ms: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
struct SessionRequest {
    runner_id: String,
    controller_id: String,
    #[serde(default)]
    broker_url: Option<String>,
    #[serde(default)]
    homeboy_version: Option<String>,
    #[serde(default)]
    homeboy_build_identity: Option<String>,
    #[serde(default)]
    worker_identity: Option<String>,
    #[serde(default)]
    worker_pid: Option<u32>,
    #[serde(default)]
    last_seen_at: Option<String>,
}

pub(super) fn route(
    method: &str,
    path: &str,
    body: Option<Value>,
    job_store: &JobStore,
) -> HttpResponse {
    match (method, path) {
        ("POST", "/runner/sessions") => match register_session(body) {
            Ok(body) => daemon_endpoint_response("runner.sessions.register", body),
            Err(err) => error_response(400, err),
        },
        ("POST", "/runner/jobs") => match enqueue(body, job_store) {
            Ok(body) => daemon_endpoint_response("runner.jobs.submit", body),
            Err(err) => error_response(400, err),
        },
        ("POST", "/runner/jobs/claim") => match claim(body, job_store) {
            Ok(body) => daemon_endpoint_response("runner.jobs.claim", body),
            Err(err) => error_response(400, err),
        },
        ("POST", path) if path.starts_with("/runner/jobs/") => update(path, body, job_store),
        _ => error_response(
            404,
            Error::validation_invalid_argument(
                "path",
                "unknown remote runner broker path",
                Some(path.to_string()),
                Some(vec![
                    "Use /runner/jobs, /runner/jobs/claim, /runner/jobs/<job-id>/events, /runner/jobs/<job-id>/finish, /runner/jobs/<job-id>/heartbeat, or /runner/jobs/<job-id>/cancel."
                        .to_string(),
                    "Use /runner/sessions to register reverse runner sessions.".to_string(),
                ]),
            ),
        ),
    }
}

fn register_session(body: Option<Value>) -> Result<Value> {
    let request: SessionRequest = parse_body(body, "remote runner session request")?;
    if request.runner_id.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "runner_id",
            "remote runner session requires a runner id",
            None,
            None,
        ));
    }
    if request.controller_id.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "controller_id",
            "remote runner session requires a controller id",
            None,
            None,
        ));
    }

    let now = chrono::Utc::now().to_rfc3339();
    let session = RunnerSession {
        runner_id: request.runner_id.clone(),
        mode: RunnerTunnelMode::Reverse,
        role: RunnerSessionRole::Controller,
        server_id: None,
        controller_id: Some(request.controller_id.clone()),
        broker_url: request.broker_url.clone(),
        remote_daemon_address: None,
        local_port: None,
        local_url: None,
        tunnel_pid: None,
        remote_daemon_pid: None,
        homeboy_version: request
            .homeboy_version
            .clone()
            .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string()),
        homeboy_build_identity: request.homeboy_build_identity.clone(),
        connected_at: now.clone(),
        worker_identity: request.worker_identity.clone(),
        worker_pid: request.worker_pid,
        last_seen_at: request.last_seen_at.clone().or(Some(now)),
    };
    let path = write_session(&session)?;

    Ok(json!({
        "command": "api.runner.sessions.register",
        "session": session,
        "session_path": path.display().to_string(),
    }))
}

fn enqueue(body: Option<Value>, job_store: &JobStore) -> Result<Value> {
    let request: RemoteRunnerJobRequest = parse_body(body, "remote runner job request")?;
    let job = job_store.submit_remote_runner_job(request.clone())?;
    Ok(json!({
        "command": "api.runner.jobs.submit",
        "job": job,
        "poll": {
            "job": format!("/jobs/{}", job.id),
            "events": format!("/jobs/{}/events", job.id),
        },
        "request": request,
    }))
}

fn claim(body: Option<Value>, job_store: &JobStore) -> Result<Value> {
    let request: ClaimRequest = parse_body(body, "remote runner claim request")?;
    touch_reverse_session(&request.runner_id)?;
    let concurrency_limit = request.concurrency_limit.or_else(|| {
        runner::load(&request.runner_id)
            .ok()
            .and_then(|runner| runner.settings.concurrency_limit)
    });
    let claim = job_store.claim_remote_runner_job(
        &request.runner_id,
        request.project_id.as_deref(),
        request.lease_ms.unwrap_or(30_000),
        concurrency_limit,
    )?;
    Ok(json!({
        "command": "api.runner.jobs.claim",
        "claim": claim,
    }))
}

fn update(path: &str, body: Option<Value>, job_store: &JobStore) -> HttpResponse {
    let Some((job_id, operation)) = job_path(path) else {
        return error_response(
            404,
            Error::validation_invalid_argument(
                "path",
                "unknown remote runner job path",
                Some(path.to_string()),
                Some(vec![
                    "Use /runner/jobs/<job-id>/events, /runner/jobs/<job-id>/finish, /runner/jobs/<job-id>/heartbeat, or /runner/jobs/<job-id>/cancel.".to_string(),
                ]),
            ),
        );
    };

    match operation {
        "events" => match append_event(job_id, body, job_store) {
            Ok(body) => daemon_endpoint_response("runner.jobs.events.append", body),
            Err(err) => error_response(400, err),
        },
        "finish" => match finish(job_id, body, job_store) {
            Ok(body) => daemon_endpoint_response("runner.jobs.finish", body),
            Err(err) => error_response(400, err),
        },
        "heartbeat" => match heartbeat(job_id, body, job_store) {
            Ok(body) => daemon_endpoint_response("runner.jobs.heartbeat", body),
            Err(err) => error_response(400, err),
        },
        "cancel" => match cancel(job_id, job_store) {
            Ok(body) => daemon_endpoint_response("runner.jobs.cancel", body),
            Err(err) => error_response(400, err),
        },
        _ => error_response(
            404,
            Error::validation_invalid_argument(
                "path",
                "unknown remote runner job operation",
                Some(operation.to_string()),
                Some(vec![
                    "Supported operations are events, finish, heartbeat, and cancel.".to_string(),
                ]),
            ),
        ),
    }
}

fn job_path(path: &str) -> Option<(Uuid, &str)> {
    let tail = path.strip_prefix("/runner/jobs/")?;
    let (job_id, operation) = tail.split_once('/')?;
    let job_id = Uuid::parse_str(job_id).ok()?;
    Some((job_id, operation))
}

fn append_event(job_id: Uuid, body: Option<Value>, job_store: &JobStore) -> Result<Value> {
    let request: EventRequest = parse_body(body, "remote runner event request")?;
    touch_reverse_session(&request.runner_id)?;
    let event = job_store.append_remote_runner_event(
        job_id,
        &request.runner_id,
        &request.claim_id,
        request.kind,
        request.message,
        request.data,
    )?;
    Ok(json!({
        "command": "api.runner.jobs.events.append",
        "event": event,
    }))
}

fn finish(job_id: Uuid, body: Option<Value>, job_store: &JobStore) -> Result<Value> {
    let request: FinishRequest = parse_body(body, "remote runner finish request")?;
    touch_reverse_session(&request.runner_id)?;
    let job = job_store.finish_remote_runner_job(
        job_id,
        &request.runner_id,
        &request.claim_id,
        request.result,
    )?;
    Ok(json!({
        "command": "api.runner.jobs.finish",
        "job": job,
    }))
}

fn heartbeat(job_id: Uuid, body: Option<Value>, job_store: &JobStore) -> Result<Value> {
    let request: HeartbeatRequest = parse_body(body, "remote runner heartbeat request")?;
    touch_reverse_session(&request.runner_id)?;
    let job = job_store.renew_remote_runner_claim(
        job_id,
        &request.runner_id,
        &request.claim_id,
        request.lease_ms.unwrap_or(30_000),
    )?;
    Ok(json!({
        "command": "api.runner.jobs.heartbeat",
        "job": job,
    }))
}

fn cancel(job_id: Uuid, job_store: &JobStore) -> Result<Value> {
    let job = job_store.cancel_remote_runner_job(job_id, "cancel requested via broker API")?;
    let events = job_store.events(job_id)?;
    Ok(json!({
        "command": "api.runner.jobs.cancel",
        "job": job,
        "events": events,
    }))
}

fn parse_body<T: for<'de> Deserialize<'de>>(body: Option<Value>, label: &str) -> Result<T> {
    serde_json::from_value(body.unwrap_or_else(|| json!({}))).map_err(|err| {
        Error::validation_invalid_argument("body", format!("invalid {label}: {err}"), None, None)
    })
}

fn touch_reverse_session(runner_id: &str) -> Result<()> {
    let path = paths::runner_session_file(runner_id)?;
    if !path.exists() {
        return Ok(());
    }
    let raw = std::fs::read_to_string(&path).map_err(|err| {
        Error::internal_io(err.to_string(), Some(format!("read {}", path.display())))
    })?;
    let mut session: RunnerSession = serde_json::from_str(&raw)
        .map_err(|err| Error::config_invalid_json(path.display().to_string(), err))?;
    if session.mode == RunnerTunnelMode::Reverse && session.role == RunnerSessionRole::Controller {
        session.last_seen_at = Some(chrono::Utc::now().to_rfc3339());
        write_session(&session)?;
    }
    Ok(())
}

fn write_session(session: &RunnerSession) -> Result<std::path::PathBuf> {
    let path = paths::runner_session_file(&session.runner_id)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|err| {
            Error::internal_io(
                err.to_string(),
                Some(format!("create {}", parent.display())),
            )
        })?;
    }
    let serialized = serde_json::to_string_pretty(session).map_err(|err| {
        Error::internal_json(
            err.to_string(),
            Some("serialize runner session".to_string()),
        )
    })?;
    std::fs::write(&path, serialized).map_err(|err| {
        Error::internal_io(err.to_string(), Some(format!("write {}", path.display())))
    })?;
    Ok(path)
}
