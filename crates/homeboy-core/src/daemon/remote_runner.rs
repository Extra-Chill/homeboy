use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{daemon_endpoint_response, error_response, HttpResponse};
use crate::api_jobs::{
    JobArtifactMetadata, JobEventKind, JobStore, RemoteRunnerJobRequest, RemoteRunnerJobResult,
};
use crate::broker_auth::{BrokerAuthStore, BrokerScope};
use crate::error::{Error, Result};
use crate::paths;
use homeboy_lab_runner_contract::{RunnerSession, RunnerSessionRole, RunnerTunnelMode};

/// Per-request broker authentication context extracted from the network layer.
///
/// `handle_connection` (the only network entry point) builds the real context
/// from request headers and the bind address. In-process callers (CLI dispatch,
/// tests) use [`BrokerAuthContext::trusted_local`], which is already inside the
/// trust boundary and bypasses bearer enforcement.
#[derive(Debug, Clone, Default)]
pub(in crate::daemon) struct BrokerAuthContext {
    pub token: Option<String>,
    pub loopback_bind: bool,
    pub trusted_local: bool,
}

impl BrokerAuthContext {
    /// Context for in-process dispatch already inside the trust boundary.
    pub(in crate::daemon) fn trusted_local() -> Self {
        Self {
            token: None,
            loopback_bind: true,
            trusted_local: true,
        }
    }

    /// Authorize this request against the on-disk broker auth store for the
    /// given scope and (optionally) the runner id carried in the request body.
    pub(in crate::daemon) fn authorize(
        &self,
        required: BrokerScope,
        runner_id: Option<&str>,
    ) -> Result<Option<crate::broker_auth::BrokerAuthGrant>> {
        if self.trusted_local {
            return Ok(None);
        }
        let store = BrokerAuthStore::load()?;
        Ok(Some(store.authorize(
            self.loopback_bind,
            self.token.as_deref(),
            required,
            runner_id,
        )?))
    }
}

#[derive(Debug, Clone, Deserialize)]
struct ClaimRequest {
    runner_id: String,
    #[serde(default)]
    project_id: Option<String>,
    #[serde(default)]
    lease_ms: Option<u64>,
    #[serde(default)]
    concurrency_limit: Option<usize>,
    #[serde(default)]
    execution_protocol: Option<crate::runner_job_execution_context::RunnerJobExecutionProtocol>,
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
struct ConsumeRequest {
    runner_id: String,
    claim_id: String,
    context_id: String,
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

pub(in crate::daemon) fn route(
    method: &str,
    path: &str,
    body: Option<Value>,
    job_store: &JobStore,
    auth: &BrokerAuthContext,
) -> HttpResponse {
    match (method, path) {
        ("POST", "/runner/sessions") => match register_session(body, auth) {
            Ok(body) => daemon_endpoint_response("runner.sessions.register", body),
            Err(err) => auth_or_bad_request(err),
        },
        ("POST", "/runner/jobs") => match enqueue(body, job_store, auth) {
            Ok(body) => daemon_endpoint_response("runner.jobs.submit", body),
            Err(err) => auth_or_bad_request(err),
        },
        ("POST", "/runner/jobs/submissions/lookup") => match submission_lookup(body, job_store, auth) {
            Ok(body) => daemon_endpoint_response("runner.jobs.submissions.lookup", body),
            Err(err) => auth_or_bad_request(err),
        },
        ("POST", "/runner/jobs/reconcile") => match reconcile(body, job_store, auth) {
            Ok(body) => daemon_endpoint_response("runner.jobs.reconcile", body),
            Err(err) => auth_or_bad_request(err),
        },
        ("POST", "/runner/jobs/claim") => match claim(body, job_store, auth) {
            Ok(body) => daemon_endpoint_response("runner.jobs.claim", body),
            Err(err) => auth_or_bad_request(err),
        },
        ("GET", path) if path.starts_with("/runner/jobs/") => lookup(path, job_store, auth),
        ("POST", path) if path.starts_with("/runner/jobs/") => update(path, body, job_store, auth),
        _ => error_response(
            404,
            Error::validation_invalid_argument(
                "path",
                "unknown remote runner broker path",
                Some(path.to_string()),
                Some(vec![
                    "Use /runner/jobs, /runner/jobs/reconcile, /runner/jobs/claim, /runner/jobs/<job-id>/events, /runner/jobs/<job-id>/finish, /runner/jobs/<job-id>/heartbeat, /runner/jobs/<job-id>/consume, /runner/jobs/<job-id>/cancel, or GET /runner/jobs/<job-id>/artifacts/<artifact-id>."
                        .to_string(),
                    "Use /runner/sessions to register reverse runner sessions.".to_string(),
                ]),
            ),
        ),
    }
}

fn lookup(path: &str, job_store: &JobStore, auth: &BrokerAuthContext) -> HttpResponse {
    if let Some((job_id, run_id)) = job_run_path(path) {
        return match lookup_run(job_id, &run_id, job_store, auth) {
            Ok(body) => daemon_endpoint_response("runner.jobs.runs.lookup", body),
            Err(err) => lookup_run_error_response(err),
        };
    }
    let Some((job_id, artifact_id, content)) = job_artifact_path(path) else {
        return error_response(
            404,
            Error::validation_invalid_argument(
                "path",
                "unknown remote runner job lookup path",
                Some(path.to_string()),
                Some(vec![
                    "Use GET /runner/jobs/<job-id>/artifacts/<artifact-id> for broker-held artifact metadata.".to_string(),
                ]),
            ),
        );
    };

    let result = if content {
        lookup_artifact_content(job_id, &artifact_id, job_store, auth)
    } else {
        lookup_artifact(job_id, &artifact_id, job_store, auth)
    };
    match result {
        Ok(body) => daemon_endpoint_response("runner.jobs.artifacts.lookup", body),
        Err(err) => lookup_run_error_response(err),
    }
}

fn lookup_run(
    job_id: Uuid,
    run_id: &str,
    job_store: &JobStore,
    auth: &BrokerAuthContext,
) -> Result<Value> {
    authorize_job_read(job_id, job_store, auth)?;
    let result = job_store
        .events(job_id)?
        .into_iter()
        .rev()
        .find(|event| event.kind == JobEventKind::Result)
        .and_then(|event| event.data)
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "run_id",
                "terminal job result is unavailable",
                Some(run_id.to_string()),
                None,
            )
        })?;
    let result: RemoteRunnerJobResult = serde_json::from_value(result).map_err(|error| {
        Error::internal_json(
            error.to_string(),
            Some("parse persisted reverse runner result".to_string()),
        )
    })?;
    result.validate_observation_run_details().map_err(|error| {
        Error::internal_json(
            error.message,
            Some("validate persisted reverse runner run details".to_string()),
        )
    })?;
    let detail = result
        .observation_run_details
        .iter()
        .find(|detail| detail.run.id == run_id)
        .cloned()
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "run_id",
                "declared observation run not found for job",
                Some(run_id.to_string()),
                None,
            )
        })?;
    Ok(json!({ "job_id": job_id.to_string(), "run": detail }))
}

fn lookup_run_error_response(err: Error) -> HttpResponse {
    let status = match err.code {
        crate::error::ErrorCode::BrokerAuthDenied => 401,
        crate::error::ErrorCode::RunnerPolicyDenied => 403,
        crate::error::ErrorCode::InternalIoError
        | crate::error::ErrorCode::InternalJsonError
        | crate::error::ErrorCode::InternalUnexpected => 500,
        _ => 404,
    };
    error_response(status, err)
}

fn job_run_path(path: &str) -> Option<(Uuid, String)> {
    let parts = path
        .strip_prefix("/runner/jobs/")?
        .split('/')
        .collect::<Vec<_>>();
    (parts.len() == 3 && parts[1] == "runs")
        .then(|| Some((Uuid::parse_str(parts[0]).ok()?, parts[2].to_string())))?
}

fn authorize_job_read(job_id: Uuid, job_store: &JobStore, auth: &BrokerAuthContext) -> Result<()> {
    // Authenticate before revealing whether the broker holds this job.
    let grant = match auth.authorize(BrokerScope::Work, None) {
        Ok(grant) => grant,
        // Controllers need to observe jobs they submit, but that must not give
        // their Submit credential any unrelated worker privileges.
        Err(_) => auth.authorize(BrokerScope::Submit, None)?,
    };
    let job = job_store.get(job_id)?;
    if let Some(grant) = grant {
        if job.target_runner_id.as_deref() != Some(grant.runner_id.as_str()) {
            return Err(Error::new(
                crate::error::ErrorCode::RunnerPolicyDenied,
                "remote runner job is not owned by this runner",
                json!({ "runner_id": grant.runner_id }),
            ));
        }
    }
    Ok(())
}

fn lookup_artifact(
    job_id: Uuid,
    artifact_id: &str,
    job_store: &JobStore,
    auth: &BrokerAuthContext,
) -> Result<Value> {
    authorize_job_read(job_id, job_store, auth)?;
    let job = job_store.get(job_id)?;
    let encoded_artifact_id = crate::execution_contract::encode_uri_component(artifact_id);
    let content_path = format!("/runner/jobs/{job_id}/artifacts/{encoded_artifact_id}/content");
    let content_available = mirrored_artifact_content(job_id, artifact_id, job_store)?
        .and_then(|artifact| artifact.content_base64)
        .is_some();
    let artifact = job
        .artifacts
        .iter()
        .find(|artifact| artifact.id == artifact_id)
        .cloned()
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "artifact_id",
                format!("remote runner artifact record not found: {artifact_id}"),
                Some(artifact_id.to_string()),
                Some(vec![
                    "Reverse broker artifact lookup exposes metadata posted with the finished job and a content path when worker-mirrored bytes are present.".to_string(),
                ]),
            )
        })?;

    Ok(json!({
        "command": "api.runner.jobs.artifacts.lookup",
        "job_id": job_id.to_string(),
        "artifact_id": artifact_id,
        "artifact": artifact,
        "retrieval": {
            "mode": "broker_content_path",
            "content_available": content_available,
            "content_url": if content_available { json!(content_path.clone()) } else { Value::Null },
            "fetch_command": if content_available { json!(format!("curl -fsS {content_path}")) } else { Value::Null },
            "metadata_path": format!("/runner/jobs/{job_id}/artifacts/{encoded_artifact_id}"),
            "content_path": content_path,
            "hint": "Reverse broker artifacts are mirrored from the worker finish payload and can be fetched through the broker content path."
        }
    }))
}

fn lookup_artifact_content(
    job_id: Uuid,
    artifact_id: &str,
    job_store: &JobStore,
    auth: &BrokerAuthContext,
) -> Result<Value> {
    authorize_job_read(job_id, job_store, auth)?;
    let artifact = mirrored_artifact_content(job_id, artifact_id, job_store)?.ok_or_else(|| {
        Error::validation_invalid_argument(
            "artifact_id",
            format!("remote runner artifact content not found: {artifact_id}"),
            Some(artifact_id.to_string()),
            Some(vec![
                "Only artifacts mirrored by the reverse worker finish payload can be fetched from the broker content path.".to_string(),
            ]),
        )
    })?;
    let content_base64 = artifact.content_base64.clone().ok_or_else(|| {
        Error::validation_invalid_argument(
            "artifact_id",
            format!("remote runner artifact content not mirrored: {artifact_id}"),
            Some(artifact_id.to_string()),
            None,
        )
    })?;
    Ok(json!({
        "command": "api.runner.jobs.artifacts.content",
        "job_id": job_id.to_string(),
        "artifact_id": artifact.id,
        "content_available": true,
        "retrieval": inline_content_retrieval(),
        "filename": artifact.name.unwrap_or_else(|| artifact_id.to_string()),
        "mime": artifact.mime,
        "size_bytes": artifact.size_bytes,
        "sha256": artifact.sha256,
        "content_base64": content_base64,
    }))
}

fn mirrored_artifact_content(
    job_id: Uuid,
    artifact_id: &str,
    job_store: &JobStore,
) -> Result<Option<JobArtifactMetadata>> {
    for event in job_store.events(job_id)?.into_iter().rev() {
        if event.kind != JobEventKind::Result {
            continue;
        }
        let Some(data) = event.data else {
            continue;
        };
        let result: RemoteRunnerJobResult = serde_json::from_value(data).map_err(|err| {
            Error::internal_json(
                err.to_string(),
                Some("parse remote runner result event".to_string()),
            )
        })?;
        if let Some(artifact) = result
            .artifacts
            .into_iter()
            .find(|artifact| artifact.id == artifact_id)
        {
            return Ok(Some(artifact));
        }
    }
    Ok(None)
}

fn inline_content_retrieval() -> Value {
    json!({
        "mode": "inline_base64",
        "content_available": true,
        "content_field": "content_base64",
        "encoding": "base64",
    })
}

#[derive(Debug, Clone, Deserialize)]
struct ReconcileRequest {
    #[serde(default)]
    runner_id: Option<String>,
}

fn reconcile(body: Option<Value>, job_store: &JobStore, auth: &BrokerAuthContext) -> Result<Value> {
    let request: ReconcileRequest = parse_body(body, "remote runner reconcile request")?;
    let grant = auth.authorize(BrokerScope::Submit, request.runner_id.as_deref())?;
    let now_ms = chrono::Utc::now().timestamp_millis().max(0) as u64;
    let reconciled = job_store.reconcile_expired_remote_runner_claims_for_runner(
        now_ms,
        grant.as_ref().map(|grant| grant.runner_id.as_str()),
    )?;
    Ok(json!({
        "command": "api.runner.jobs.reconcile",
        "reconciled": reconciled,
        "reconciled_count": reconciled.len(),
        "policy": {
            "owner": "broker",
            "reason": "expired reverse-runner claims are broker-owned lifecycle state"
        },
    }))
}

fn register_session(body: Option<Value>, auth: &BrokerAuthContext) -> Result<Value> {
    let request: SessionRequest = parse_body(body, "remote runner session request")?;
    if request.runner_id.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "runner_id",
            "remote runner session requires a runner id",
            None,
            None,
        ));
    }
    auth.authorize(BrokerScope::Work, Some(request.runner_id.as_str()))?;
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
        remote_daemon_lease_id: None,
        homeboy_version: request
            .homeboy_version
            .clone()
            .unwrap_or_else(|| crate::build_identity::current().version),
        homeboy_build_identity: request.homeboy_build_identity.clone(),
        connected_at: now.clone(),
        worker_identity: request.worker_identity.clone(),
        worker_pid: request.worker_pid,
        last_seen_at: request.last_seen_at.clone().or(Some(now)),
        leaseless_recovery_evidence: None,
    };
    let path = write_session(&session)?;

    Ok(json!({
        "command": "api.runner.sessions.register",
        "session": session,
        "session_path": path.display().to_string(),
    }))
}

fn enqueue(body: Option<Value>, job_store: &JobStore, auth: &BrokerAuthContext) -> Result<Value> {
    let mut request: RemoteRunnerJobRequest = parse_body(body, "remote runner job request")?;
    auth.authorize(BrokerScope::Submit, Some(request.runner_id.as_str()))?;
    request.normalize();
    let public_request = request.public_metadata();
    let job = job_store.submit_remote_runner_job(request)?;
    Ok(json!({
        "command": "api.runner.jobs.submit",
        "job": job,
        "poll": {
            "job": format!("/jobs/{}", job.id),
            "events": format!("/jobs/{}/events", job.id),
        },
        "request": public_request,
    }))
}

#[derive(Deserialize)]
struct SubmissionLookupRequest {
    runner_id: String,
    submission_key: String,
}

fn submission_lookup(
    body: Option<Value>,
    job_store: &JobStore,
    auth: &BrokerAuthContext,
) -> Result<Value> {
    let request: SubmissionLookupRequest = parse_body(body, "remote runner submission lookup")?;
    let grant = auth.authorize(BrokerScope::Submit, Some(request.runner_id.as_str()))?;
    let result = job_store.lookup_remote_runner_submission(&request.submission_key);
    if let crate::api_jobs::RemoteRunnerSubmissionLookup::Accepted { job } = &result {
        if let Some(grant) = grant {
            if job.target_runner_id.as_deref() != Some(grant.runner_id.as_str()) {
                return Err(Error::new(
                    crate::error::ErrorCode::RunnerPolicyDenied,
                    "remote runner submission is not owned by this runner",
                    json!({ "runner_id": grant.runner_id }),
                ));
            }
        }
    }
    Ok(json!({
        "command": "api.runner.jobs.submissions.lookup",
        "result": result,
    }))
}

fn claim(body: Option<Value>, job_store: &JobStore, auth: &BrokerAuthContext) -> Result<Value> {
    let request: ClaimRequest = parse_body(body, "remote runner claim request")?;
    auth.authorize(BrokerScope::Work, Some(request.runner_id.as_str()))?;
    touch_reverse_session(&request.runner_id)?;
    let concurrency_limit = request
        .concurrency_limit
        .or_else(|| super::runner_workspace_root::runner_concurrency_limit(&request.runner_id));
    let claim = job_store.claim_remote_runner_job_with_execution_protocol(
        &request.runner_id,
        request.project_id.as_deref(),
        request.lease_ms.unwrap_or(30_000),
        concurrency_limit,
        request.execution_protocol.as_ref(),
    )?;
    Ok(json!({
        "command": "api.runner.jobs.claim",
        "claim": claim,
    }))
}

fn update(
    path: &str,
    body: Option<Value>,
    job_store: &JobStore,
    auth: &BrokerAuthContext,
) -> HttpResponse {
    let Some((job_id, operation)) = job_path(path) else {
        return error_response(
            404,
            Error::validation_invalid_argument(
                "path",
                "unknown remote runner job path",
                Some(path.to_string()),
                Some(vec![
                    "Use /runner/jobs/<job-id>/events, /runner/jobs/<job-id>/finish, /runner/jobs/<job-id>/heartbeat, /runner/jobs/<job-id>/consume, or /runner/jobs/<job-id>/cancel.".to_string(),
                ]),
            ),
        );
    };

    match operation {
        "events" => match append_event(job_id, body, job_store, auth) {
            Ok(body) => daemon_endpoint_response("runner.jobs.events.append", body),
            Err(err) => auth_or_bad_request(err),
        },
        "finish" => match finish(job_id, body, job_store, auth) {
            Ok(body) => daemon_endpoint_response("runner.jobs.finish", body),
            Err(err) => auth_or_bad_request(err),
        },
        "heartbeat" => match heartbeat(job_id, body, job_store, auth) {
            Ok(body) => daemon_endpoint_response("runner.jobs.heartbeat", body),
            Err(err) => auth_or_bad_request(err),
        },
        "consume" => match consume(job_id, body, job_store, auth) {
            Ok(body) => daemon_endpoint_response("runner.jobs.consume", body),
            Err(err) => auth_or_bad_request(err),
        },
        "cancel" => match cancel(job_id, job_store, auth) {
            Ok(body) => daemon_endpoint_response("runner.jobs.cancel", body),
            Err(err) => auth_or_bad_request(err),
        },
        _ => error_response(
            404,
            Error::validation_invalid_argument(
                "path",
                "unknown remote runner job operation",
                Some(operation.to_string()),
                Some(vec![
                    "Supported operations are events, finish, heartbeat, consume, and cancel."
                        .to_string(),
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

fn job_artifact_path(path: &str) -> Option<(Uuid, String, bool)> {
    let tail = path.strip_prefix("/runner/jobs/")?;
    let (job_id, tail) = tail.split_once('/')?;
    let artifact_id = tail.strip_prefix("artifacts/")?;
    let (artifact_id, content) = if let Some(artifact_id) = artifact_id.strip_suffix("/content") {
        (artifact_id, true)
    } else {
        (artifact_id, false)
    };
    if artifact_id.is_empty() || artifact_id.contains('/') {
        return None;
    }
    let job_id = Uuid::parse_str(job_id).ok()?;
    Some((
        job_id,
        crate::execution_contract::decode_uri_component(artifact_id),
        content,
    ))
}

fn append_event(
    job_id: Uuid,
    body: Option<Value>,
    job_store: &JobStore,
    auth: &BrokerAuthContext,
) -> Result<Value> {
    let request: EventRequest = parse_body(body, "remote runner event request")?;
    auth.authorize(BrokerScope::Work, Some(request.runner_id.as_str()))?;
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

fn finish(
    job_id: Uuid,
    body: Option<Value>,
    job_store: &JobStore,
    auth: &BrokerAuthContext,
) -> Result<Value> {
    let request: FinishRequest = parse_body(body, "remote runner finish request")?;
    auth.authorize(BrokerScope::Work, Some(request.runner_id.as_str()))?;
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

fn heartbeat(
    job_id: Uuid,
    body: Option<Value>,
    job_store: &JobStore,
    auth: &BrokerAuthContext,
) -> Result<Value> {
    let request: HeartbeatRequest = parse_body(body, "remote runner heartbeat request")?;
    auth.authorize(BrokerScope::Work, Some(request.runner_id.as_str()))?;
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

fn consume(
    job_id: Uuid,
    body: Option<Value>,
    job_store: &JobStore,
    auth: &BrokerAuthContext,
) -> Result<Value> {
    let request: ConsumeRequest = parse_body(body, "remote runner execution consume request")?;
    auth.authorize(BrokerScope::Work, Some(request.runner_id.as_str()))?;
    touch_reverse_session(&request.runner_id)?;
    let job = job_store.consume_remote_runner_execution(
        job_id,
        &request.runner_id,
        &request.claim_id,
        &request.context_id,
    )?;
    Ok(json!({
        "command": "api.runner.jobs.consume",
        "job": job,
        "context_id": request.context_id,
    }))
}

fn cancel(job_id: Uuid, job_store: &JobStore, auth: &BrokerAuthContext) -> Result<Value> {
    authorize_job_submit(job_id, job_store, auth)?;
    let job = job_store.cancel_remote_runner_job(job_id, "cancel requested via broker API")?;
    let events = job_store.events(job_id)?;
    Ok(json!({
        "command": "api.runner.jobs.cancel",
        "job": job,
        "events": events,
    }))
}

fn authorize_job_submit(
    job_id: Uuid,
    job_store: &JobStore,
    auth: &BrokerAuthContext,
) -> Result<()> {
    let grant = auth.authorize(BrokerScope::Submit, None)?;
    let job = job_store.get(job_id)?;
    if let Some(grant) = grant {
        if job.target_runner_id.as_deref() != Some(grant.runner_id.as_str()) {
            return Err(Error::new(
                crate::error::ErrorCode::RunnerPolicyDenied,
                "remote runner job is not owned by this submitting controller",
                json!({ "runner_id": grant.runner_id }),
            ));
        }
    }
    Ok(())
}

/// Map a handler error to an HTTP response. Broker auth rejections become
/// `401 Unauthorized` (so unauthenticated callers see a distinct status), all
/// other errors keep the existing `400 Bad Request` contract.
pub(in crate::daemon) fn auth_or_bad_request(err: Error) -> HttpResponse {
    if err.code == crate::error::ErrorCode::BrokerAuthDenied {
        error_response(401, err)
    } else {
        error_response(400, err)
    }
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

#[cfg(test)]
mod auth_tests {
    use super::*;
    use crate::broker_auth::BrokerScope;
    use crate::test_support::HomeGuard;
    use std::collections::BTreeSet;

    /// Build a network-style (enforcing, non-loopback) auth context carrying
    /// `token`. `trusted_local` is false so the auth store is consulted.
    fn enforcing_auth(token: Option<&str>) -> BrokerAuthContext {
        BrokerAuthContext {
            token: token.map(str::to_string),
            loopback_bind: false,
            trusted_local: false,
        }
    }

    /// Pair a runner credential with `scope`, returning the one-time token.
    fn pair(runner_id: &str, scope: BrokerScope) -> String {
        pair_extra("cred-1", runner_id, scope)
    }

    /// Pair an additional credential under an explicit `id`.
    fn pair_extra(id: &str, runner_id: &str, scope: BrokerScope) -> String {
        let mut store = BrokerAuthStore::load().expect("load store");
        let scopes: BTreeSet<BrokerScope> = std::iter::once(scope).collect();
        let minted = store.pair(id, runner_id, scopes).expect("pair");
        store.save().expect("save store");
        minted.token
    }

    fn submit_body() -> Value {
        json!({
            "runner_id": "homeboy-lab",
            "project_id": "extrachill",
            "command": ["homeboy", "test", "sample"],
            "cwd": "/tmp/sample"
        })
    }

    fn typed_result(run_id: &str) -> Value {
        json!({
            "exit_code": 0,
            "observation_run_ids": [run_id],
            "observation_run_details": [{
                "schema": "homeboy/remote-runner-observation-run-detail/v1",
                "run": {
                    "id": run_id,
                    "kind": "test",
                    "component_id": null,
                    "started_at": "2026-01-01T00:00:00Z",
                    "finished_at": "2026-01-01T00:01:00Z",
                    "status": "succeeded",
                    "command": null,
                    "cwd": null,
                    "homeboy_version": null,
                    "git_sha": null,
                    "rig_id": null,
                    "metadata_json": {}
                },
                "artifacts": []
            }]
        })
    }

    fn terminal_typed_job(store: &JobStore, runner_id: &str, run_id: &str) -> String {
        let submit = route(
            "POST",
            "/runner/jobs",
            Some(json!({
                "runner_id": runner_id,
                "command": ["homeboy", "test"],
                "cwd": "/tmp/x"
            })),
            store,
            &BrokerAuthContext::trusted_local(),
        );
        let job_id = submit.body["body"]["job"]["id"]
            .as_str()
            .expect("job id")
            .to_string();
        let claim = route(
            "POST",
            "/runner/jobs/claim",
            Some(json!({ "runner_id": runner_id, "lease_ms": 30000 })),
            store,
            &BrokerAuthContext::trusted_local(),
        );
        let claim_id = claim.body["body"]["claim"]["job"]["claim_id"]
            .as_str()
            .expect("claim id")
            .to_string();
        let finish = route(
            "POST",
            &format!("/runner/jobs/{job_id}/finish"),
            Some(json!({
                "runner_id": runner_id,
                "claim_id": claim_id,
                "result": typed_result(run_id)
            })),
            store,
            &BrokerAuthContext::trusted_local(),
        );
        assert_eq!(finish.status_code, 200, "finish body: {}", finish.body);
        job_id
    }

    #[test]
    fn run_detail_requires_a_bearer_token() {
        let _home = HomeGuard::new();
        pair("runner-a", BrokerScope::Work);
        let response = route(
            "GET",
            &format!("/runner/jobs/{}/runs/run-1", Uuid::new_v4()),
            None,
            &JobStore::default(),
            &enforcing_auth(None),
        );
        assert_eq!(response.status_code, 401);
        assert_eq!(response.body["error"], "broker.auth_denied");
    }

    #[test]
    fn run_detail_rejects_an_invalid_bearer_token() {
        let _home = HomeGuard::new();
        pair("runner-a", BrokerScope::Work);
        let response = route(
            "GET",
            &format!("/runner/jobs/{}/runs/run-1", Uuid::new_v4()),
            None,
            &JobStore::default(),
            &enforcing_auth(Some("not-a-paired-token")),
        );
        assert_eq!(response.status_code, 401);
        assert_eq!(response.body["error"], "broker.auth_denied");
    }

    #[test]
    fn run_detail_rejects_a_valid_token_for_a_different_runner() {
        let _home = HomeGuard::new();
        let token = pair("runner-b", BrokerScope::Work);
        let store = JobStore::default();
        let job_id = terminal_typed_job(&store, "runner-a", "run-1");

        let response = route(
            "GET",
            &format!("/runner/jobs/{job_id}/runs/run-not-owned"),
            None,
            &store,
            &enforcing_auth(Some(&token)),
        );
        assert_eq!(response.status_code, 403);
        assert_eq!(response.body["error"], "runner.policy_denied");
    }

    #[test]
    fn run_detail_returns_not_found_for_a_missing_job_or_run() {
        let _home = HomeGuard::new();
        let token = pair("runner-a", BrokerScope::Work);
        let store = JobStore::default();
        let missing_job = route(
            "GET",
            &format!("/runner/jobs/{}/runs/run-1", Uuid::new_v4()),
            None,
            &store,
            &enforcing_auth(Some(&token)),
        );
        assert_eq!(missing_job.status_code, 404);

        let job_id = terminal_typed_job(&store, "runner-a", "run-1");
        let missing_run = route(
            "GET",
            &format!("/runner/jobs/{job_id}/runs/run-missing"),
            None,
            &store,
            &enforcing_auth(Some(&token)),
        );
        assert_eq!(missing_run.status_code, 404);
    }

    #[test]
    fn run_detail_returns_typed_details_to_the_owning_runner() {
        let _home = HomeGuard::new();
        let token = pair("runner-a", BrokerScope::Work);
        let store = JobStore::default();
        let job_id = terminal_typed_job(&store, "runner-a", "run-1");

        let response = route(
            "GET",
            &format!("/runner/jobs/{job_id}/runs/run-1"),
            None,
            &store,
            &enforcing_auth(Some(&token)),
        );
        assert_eq!(
            response.status_code, 200,
            "response body: {}",
            response.body
        );
        assert_eq!(
            response.body["body"]["run"]["schema"],
            "homeboy/remote-runner-observation-run-detail/v1"
        );
        assert_eq!(response.body["body"]["run"]["run"]["id"], "run-1");
    }

    #[test]
    fn submit_token_reads_its_own_job_but_not_another_runners_job() {
        let _home = HomeGuard::new();
        let submit_token = pair("runner-a", BrokerScope::Submit);
        let store = JobStore::default();
        let owned_job = terminal_typed_job(&store, "runner-a", "run-1");
        let foreign_job = terminal_typed_job(&store, "runner-b", "run-2");

        let owned = route(
            "GET",
            &format!("/runner/jobs/{owned_job}/runs/run-1"),
            None,
            &store,
            &enforcing_auth(Some(&submit_token)),
        );
        assert_eq!(owned.status_code, 200);

        let foreign = route(
            "GET",
            &format!("/runner/jobs/{foreign_job}/runs/run-2"),
            None,
            &store,
            &enforcing_auth(Some(&submit_token)),
        );
        assert_eq!(foreign.status_code, 403);
        assert_eq!(foreign.body["error"], "runner.policy_denied");
    }

    #[test]
    fn artifact_routes_require_the_owning_work_bearer_and_preserve_not_found() {
        let _home = HomeGuard::new();
        let owner_token = pair("runner-a", BrokerScope::Work);
        let other_token = pair_extra("runner-b-work", "runner-b", BrokerScope::Work);
        let store = JobStore::default();
        let job_id = terminal_typed_job(&store, "runner-a", "run-1");

        for suffix in ["artifacts/missing", "artifacts/missing/content"] {
            let path = format!("/runner/jobs/{job_id}/{suffix}");
            let unauthenticated = route("GET", &path, None, &store, &enforcing_auth(None));
            assert_eq!(unauthenticated.status_code, 401, "{suffix}");
            assert_eq!(unauthenticated.body["error"], "broker.auth_denied");

            let foreign = route(
                "GET",
                &path,
                None,
                &store,
                &enforcing_auth(Some(&other_token)),
            );
            assert_eq!(foreign.status_code, 403, "{suffix}");
            assert_eq!(foreign.body["error"], "runner.policy_denied");

            let missing = route(
                "GET",
                &path,
                None,
                &store,
                &enforcing_auth(Some(&owner_token)),
            );
            assert_eq!(missing.status_code, 404, "{suffix}");
        }
    }

    #[test]
    fn run_detail_reports_malformed_persisted_details_as_a_server_error() {
        let _home = HomeGuard::new();
        let token = pair("runner-a", BrokerScope::Work);
        let store = JobStore::default();
        let submit = route(
            "POST",
            "/runner/jobs",
            Some(json!({
                "runner_id": "runner-a",
                "command": ["homeboy", "test"],
                "cwd": "/tmp/x"
            })),
            &store,
            &BrokerAuthContext::trusted_local(),
        );
        let job_id = submit.body["body"]["job"]["id"]
            .as_str()
            .expect("job id")
            .to_string();
        let mut malformed = typed_result("run-1");
        malformed["observation_run_details"][0]["schema"] = json!("invalid-schema");
        store
            .append_event(
                Uuid::parse_str(&job_id).expect("valid job id"),
                JobEventKind::Result,
                None,
                Some(malformed),
            )
            .expect("persist malformed result for lookup regression coverage");

        let response = route(
            "GET",
            &format!("/runner/jobs/{job_id}/runs/run-1"),
            None,
            &store,
            &enforcing_auth(Some(&token)),
        );
        assert_eq!(response.status_code, 500);
        assert_eq!(response.body["error"], "internal.json_error");
    }

    #[test]
    fn unauthenticated_broker_route_is_rejected() {
        let _home = HomeGuard::new();
        // Configure at least one credential so the broker is in enforcing mode.
        pair("homeboy-lab", BrokerScope::Work);
        let store = JobStore::default();

        let response = route(
            "POST",
            "/runner/jobs/claim",
            Some(json!({ "runner_id": "homeboy-lab", "lease_ms": 30000 })),
            &store,
            &enforcing_auth(None),
        );
        assert_eq!(response.status_code, 401);
        assert_eq!(response.body["error"], "broker.auth_denied");
    }

    #[test]
    fn paired_runner_can_register_claim_progress_and_finish() {
        let _home = HomeGuard::new();
        let submit_token = pair("homeboy-lab", BrokerScope::Submit);
        // Add a work-scoped credential for the worker side.
        let work_token = pair_extra("worker-cred", "homeboy-lab", BrokerScope::Work);

        let store = JobStore::default();

        // Controller submits (submit scope).
        let submit = route(
            "POST",
            "/runner/jobs",
            Some(submit_body()),
            &store,
            &enforcing_auth(Some(&submit_token)),
        );
        assert_eq!(submit.status_code, 200, "submit body: {}", submit.body);
        let job_id = submit.body["body"]["job"]["id"]
            .as_str()
            .expect("job id")
            .to_string();

        // Worker claims (work scope, runner-bound).
        let claim = route(
            "POST",
            "/runner/jobs/claim",
            Some(json!({ "runner_id": "homeboy-lab", "lease_ms": 30000 })),
            &store,
            &enforcing_auth(Some(&work_token)),
        );
        assert_eq!(claim.status_code, 200, "claim body: {}", claim.body);
        let claim_id = claim.body["body"]["claim"]["job"]["claim_id"]
            .as_str()
            .expect("claim id")
            .to_string();

        // Worker streams progress.
        let event = route(
            "POST",
            &format!("/runner/jobs/{job_id}/events"),
            Some(json!({
                "runner_id": "homeboy-lab",
                "claim_id": claim_id,
                "kind": "progress",
                "message": "started"
            })),
            &store,
            &enforcing_auth(Some(&work_token)),
        );
        assert_eq!(event.status_code, 200, "event body: {}", event.body);

        // Worker finishes.
        let finish = route(
            "POST",
            &format!("/runner/jobs/{job_id}/finish"),
            Some(json!({
                "runner_id": "homeboy-lab",
                "claim_id": claim_id,
                "result": { "exit_code": 0 }
            })),
            &store,
            &enforcing_auth(Some(&work_token)),
        );
        assert_eq!(finish.status_code, 200, "finish body: {}", finish.body);
        assert_eq!(finish.body["body"]["job"]["status"], "succeeded");
    }

    #[test]
    fn broker_artifact_content_path_returns_worker_mirrored_bytes() {
        let _home = HomeGuard::new();
        let store = JobStore::default();
        let submit = route(
            "POST",
            "/runner/jobs",
            Some(submit_body()),
            &store,
            &BrokerAuthContext::trusted_local(),
        );
        assert_eq!(submit.status_code, 200, "submit body: {}", submit.body);
        let job_id = submit.body["body"]["job"]["id"]
            .as_str()
            .expect("job id")
            .to_string();
        let claim = route(
            "POST",
            "/runner/jobs/claim",
            Some(json!({ "runner_id": "homeboy-lab", "lease_ms": 30000 })),
            &store,
            &BrokerAuthContext::trusted_local(),
        );
        assert_eq!(claim.status_code, 200, "claim body: {}", claim.body);
        let claim_id = claim.body["body"]["claim"]["job"]["claim_id"]
            .as_str()
            .expect("claim id")
            .to_string();

        let finish = route(
            "POST",
            &format!("/runner/jobs/{job_id}/finish"),
            Some(json!({
                "runner_id": "homeboy-lab",
                "claim_id": claim_id,
                "result": {
                    "exit_code": 0,
                    "artifacts": [{
                        "id": "report.txt",
                        "name": "report.txt",
                        "path": "/worker/report.txt",
                        "mime": "text/plain",
                        "size_bytes": 21,
                        "content_base64": "d29ya2VyIGFydGlmYWN0IGJ5dGVz"
                    }]
                }
            })),
            &store,
            &BrokerAuthContext::trusted_local(),
        );
        assert_eq!(finish.status_code, 200, "finish body: {}", finish.body);
        assert!(finish.body["body"]["job"]["artifacts"][0]
            .get("content_base64")
            .is_none());

        let metadata = route(
            "GET",
            &format!("/runner/jobs/{job_id}/artifacts/report.txt"),
            None,
            &store,
            &BrokerAuthContext::trusted_local(),
        );
        assert_eq!(
            metadata.status_code, 200,
            "metadata body: {}",
            metadata.body
        );
        assert_eq!(
            metadata.body["body"]["retrieval"]["content_available"],
            json!(true)
        );
        assert_eq!(
            metadata.body["body"]["retrieval"]["content_path"],
            json!(format!(
                "/runner/jobs/{job_id}/artifacts/report.txt/content"
            ))
        );

        let content = route(
            "GET",
            &format!("/runner/jobs/{job_id}/artifacts/report.txt/content"),
            None,
            &store,
            &BrokerAuthContext::trusted_local(),
        );
        assert_eq!(content.status_code, 200, "content body: {}", content.body);
        assert_eq!(content.body["body"]["content_available"], json!(true));
        assert_eq!(
            content.body["body"]["retrieval"]["mode"],
            json!("inline_base64")
        );
        assert_eq!(
            content.body["body"]["content_base64"],
            json!("d29ya2VyIGFydGlmYWN0IGJ5dGVz")
        );
    }

    #[test]
    fn broker_retains_every_declared_run_artifact_after_reverse_worker_completion() {
        let _home = HomeGuard::new();
        let store = JobStore::default();
        let submit = route(
            "POST",
            "/runner/jobs",
            Some(submit_body()),
            &store,
            &BrokerAuthContext::trusted_local(),
        );
        let job_id = submit.body["body"]["job"]["id"]
            .as_str()
            .expect("job id")
            .to_string();
        let claim = route(
            "POST",
            "/runner/jobs/claim",
            Some(json!({ "runner_id": "homeboy-lab", "lease_ms": 30000 })),
            &store,
            &BrokerAuthContext::trusted_local(),
        );
        let claim_id = claim.body["body"]["claim"]["job"]["claim_id"]
            .as_str()
            .expect("claim id")
            .to_string();
        let artifacts = [
            ("direct-first", "ZGlyZWN0IGZpcnN0"),
            ("direct-second", "ZGlyZWN0IHNlY29uZA=="),
        ];
        let detail_artifacts = artifacts
            .iter()
            .map(|(id, _)| {
                json!({
                    "id": id,
                    "run_id": "declared-run",
                    "kind": "test_output",
                    "artifact_type": "file",
                    "path": format!("/worker/{id}"),
                    "metadata_json": {},
                    "created_at": "2026-01-01T00:01:00Z"
                })
            })
            .collect::<Vec<_>>();
        let transported_artifacts = artifacts
            .iter()
            .map(|(id, content_base64)| {
                json!({
                    "id": id,
                    "name": format!("{id}.txt"),
                    "path": format!("/worker/{id}"),
                    "mime": "text/plain",
                    "size_bytes": 12,
                    "content_base64": content_base64
                })
            })
            .collect::<Vec<_>>();
        let finish = route(
            "POST",
            &format!("/runner/jobs/{job_id}/finish"),
            Some(json!({
                "runner_id": "homeboy-lab",
                "claim_id": claim_id,
                "result": {
                    "exit_code": 0,
                    "observation_run_ids": ["declared-run"],
                    "observation_run_details": [{
                        "schema": "homeboy/remote-runner-observation-run-detail/v1",
                        "run": {
                            "id": "declared-run", "kind": "test", "component_id": null,
                            "started_at": "2026-01-01T00:00:00Z", "finished_at": "2026-01-01T00:01:00Z",
                            "status": "pass", "command": null, "cwd": null, "homeboy_version": null,
                            "git_sha": null, "rig_id": null, "metadata_json": {}
                        },
                        "artifacts": detail_artifacts
                    }],
                    "artifacts": transported_artifacts
                }
            })),
            &store,
            &BrokerAuthContext::trusted_local(),
        );
        assert_eq!(finish.status_code, 200, "finish body: {}", finish.body);

        for (id, direct_output) in artifacts {
            let content = route(
                "GET",
                &format!("/runner/jobs/{job_id}/artifacts/{id}/content"),
                None,
                &store,
                &BrokerAuthContext::trusted_local(),
            );
            assert_eq!(content.status_code, 200, "content body: {}", content.body);
            assert_eq!(content.body["body"]["content_base64"], direct_output);
        }
    }

    #[test]
    fn wrong_runner_id_cannot_claim_anothers_jobs() {
        let _home = HomeGuard::new();
        // Token paired to runner-a only.
        let token = pair("runner-a", BrokerScope::Work);
        let store = JobStore::default();

        // Attempt to claim as runner-b with runner-a's token.
        let claim = route(
            "POST",
            "/runner/jobs/claim",
            Some(json!({ "runner_id": "runner-b", "lease_ms": 30000 })),
            &store,
            &enforcing_auth(Some(&token)),
        );
        assert_eq!(claim.status_code, 401);
        assert_eq!(claim.body["error"], "broker.auth_denied");
    }

    #[test]
    fn wrong_runner_id_cannot_finish_anothers_job() {
        let _home = HomeGuard::new();
        // Submit + claim legitimately as runner-a.
        let submit_token = pair("runner-a", BrokerScope::Submit);
        let work_token = pair_extra("work-a", "runner-a", BrokerScope::Work);
        // A second runner with its own (work) token.
        let other_token = pair_extra("work-b", "runner-b", BrokerScope::Work);

        let store = JobStore::default();
        let submit = route(
            "POST",
            "/runner/jobs",
            Some(json!({
                "runner_id": "runner-a",
                "command": ["homeboy", "test"],
                "cwd": "/tmp/x"
            })),
            &store,
            &enforcing_auth(Some(&submit_token)),
        );
        let job_id = submit.body["body"]["job"]["id"]
            .as_str()
            .unwrap()
            .to_string();
        let claim = route(
            "POST",
            "/runner/jobs/claim",
            Some(json!({ "runner_id": "runner-a", "lease_ms": 30000 })),
            &store,
            &enforcing_auth(Some(&work_token)),
        );
        let claim_id = claim.body["body"]["claim"]["job"]["claim_id"]
            .as_str()
            .unwrap()
            .to_string();

        // runner-b tries to finish runner-a's job with its own valid token.
        let finish = route(
            "POST",
            &format!("/runner/jobs/{job_id}/finish"),
            Some(json!({
                "runner_id": "runner-b",
                "claim_id": claim_id,
                "result": { "exit_code": 0 }
            })),
            &store,
            &enforcing_auth(Some(&other_token)),
        );
        assert_eq!(finish.status_code, 401);
        assert_eq!(finish.body["error"], "broker.auth_denied");
    }

    #[test]
    fn work_token_cannot_submit_jobs() {
        let _home = HomeGuard::new();
        let work_token = pair("homeboy-lab", BrokerScope::Work);
        let store = JobStore::default();
        let submit = route(
            "POST",
            "/runner/jobs",
            Some(submit_body()),
            &store,
            &enforcing_auth(Some(&work_token)),
        );
        assert_eq!(submit.status_code, 401);
        assert_eq!(submit.body["error"], "broker.auth_denied");
    }
}
