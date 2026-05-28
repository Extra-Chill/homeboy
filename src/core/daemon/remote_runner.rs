use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{daemon_endpoint_response, error_response, HttpResponse};
use crate::core::api_jobs::{
    JobEventKind, JobStore, RemoteRunnerJobRequest, RemoteRunnerJobResult,
};
use crate::core::error::{Error, Result};

#[derive(Debug, Clone, Deserialize)]
struct ClaimRequest {
    runner_id: String,
    #[serde(default)]
    project_id: Option<String>,
    #[serde(default)]
    lease_ms: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
struct EventRequest {
    runner_id: String,
    kind: JobEventKind,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    data: Option<Value>,
}

#[derive(Debug, Clone, Deserialize)]
struct FinishRequest {
    runner_id: String,
    result: RemoteRunnerJobResult,
}

pub(super) fn route(
    method: &str,
    path: &str,
    body: Option<Value>,
    job_store: &JobStore,
) -> HttpResponse {
    match (method, path) {
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
                    "Use /runner/jobs, /runner/jobs/claim, /runner/jobs/<job-id>/events, or /runner/jobs/<job-id>/finish."
                        .to_string(),
                ]),
            ),
        ),
    }
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
    let claim = job_store.claim_remote_runner_job(
        &request.runner_id,
        request.project_id.as_deref(),
        request.lease_ms.unwrap_or(30_000),
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
                    "Use /runner/jobs/<job-id>/events or /runner/jobs/<job-id>/finish.".to_string(),
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
        _ => error_response(
            404,
            Error::validation_invalid_argument(
                "path",
                "unknown remote runner job operation",
                Some(operation.to_string()),
                Some(vec![
                    "Supported operations are events and finish.".to_string()
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
    let event = job_store.append_remote_runner_event(
        job_id,
        &request.runner_id,
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
    let job = job_store.finish_remote_runner_job(job_id, &request.runner_id, request.result)?;
    Ok(json!({
        "command": "api.runner.jobs.finish",
        "job": job,
    }))
}

fn parse_body<T: for<'de> Deserialize<'de>>(body: Option<Value>, label: &str) -> Result<T> {
    serde_json::from_value(body.unwrap_or_else(|| json!({}))).map_err(|err| {
        Error::validation_invalid_argument("body", format!("invalid {label}: {err}"), None, None)
    })
}
