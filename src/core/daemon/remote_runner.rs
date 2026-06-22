use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{daemon_endpoint_response, error_response, HttpResponse};
use crate::core::api_jobs::{
    JobEventKind, JobStore, RemoteRunnerJobRequest, RemoteRunnerJobResult,
};
use crate::core::error::{Error, Result};
use crate::core::paths;
use crate::core::runner::{
    self, BrokerAuthStore, BrokerScope, RunnerSession, RunnerSessionRole, RunnerTunnelMode,
};

/// Per-request broker authentication context extracted from the network layer.
///
/// `handle_connection` (the only network entry point) builds the real context
/// from request headers and the bind address. In-process callers (CLI dispatch,
/// tests) use [`BrokerAuthContext::trusted_local`], which is already inside the
/// trust boundary and bypasses bearer enforcement.
#[derive(Debug, Clone, Default)]
pub(in crate::core::daemon) struct BrokerAuthContext {
    pub token: Option<String>,
    pub loopback_bind: bool,
    pub trusted_local: bool,
}

impl BrokerAuthContext {
    /// Context for in-process dispatch already inside the trust boundary.
    pub(in crate::core::daemon) fn trusted_local() -> Self {
        Self {
            token: None,
            loopback_bind: true,
            trusted_local: true,
        }
    }

    /// Authorize this request against the on-disk broker auth store for the
    /// given scope and (optionally) the runner id carried in the request body.
    fn authorize(&self, required: BrokerScope, runner_id: Option<&str>) -> Result<()> {
        if self.trusted_local {
            return Ok(());
        }
        let store = BrokerAuthStore::load()?;
        store.authorize(
            self.loopback_bind,
            self.token.as_deref(),
            required,
            runner_id,
        )?;
        Ok(())
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

pub(in crate::core::daemon) fn route(
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
        ("POST", "/runner/jobs/reconcile") => match reconcile(job_store) {
            Ok(body) => daemon_endpoint_response("runner.jobs.reconcile", body),
            Err(err) => error_response(400, err),
        },
        ("POST", "/runner/jobs/claim") => match claim(body, job_store, auth) {
            Ok(body) => daemon_endpoint_response("runner.jobs.claim", body),
            Err(err) => auth_or_bad_request(err),
        },
        ("GET", path) if path.starts_with("/runner/jobs/") => lookup(path, job_store),
        ("POST", path) if path.starts_with("/runner/jobs/") => update(path, body, job_store, auth),
        _ => error_response(
            404,
            Error::validation_invalid_argument(
                "path",
                "unknown remote runner broker path",
                Some(path.to_string()),
                Some(vec![
                    "Use /runner/jobs, /runner/jobs/reconcile, /runner/jobs/claim, /runner/jobs/<job-id>/events, /runner/jobs/<job-id>/finish, /runner/jobs/<job-id>/heartbeat, /runner/jobs/<job-id>/cancel, or GET /runner/jobs/<job-id>/artifacts/<artifact-id>."
                        .to_string(),
                    "Use /runner/sessions to register reverse runner sessions.".to_string(),
                ]),
            ),
        ),
    }
}

fn lookup(path: &str, job_store: &JobStore) -> HttpResponse {
    let Some((job_id, artifact_id)) = job_artifact_path(path) else {
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

    match lookup_artifact(job_id, &artifact_id, job_store) {
        Ok(body) => daemon_endpoint_response("runner.jobs.artifacts.lookup", body),
        Err(err) => error_response(404, err),
    }
}

fn lookup_artifact(job_id: Uuid, artifact_id: &str, job_store: &JobStore) -> Result<Value> {
    let job = job_store.get(job_id)?;
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
                    "Reverse broker artifact lookup exposes metadata posted with the finished job; artifact bytes are not stored by the broker yet.".to_string(),
                ]),
            )
        })?;

    Ok(json!({
        "command": "api.runner.jobs.artifacts.lookup",
        "job_id": job_id.to_string(),
        "artifact_id": artifact_id,
        "artifact": artifact,
        "retrieval": {
            "mode": "metadata_only",
            "content_available": false,
            "content_url": null,
            "fetch_command": null,
            "metadata_path": format!("/runner/jobs/{job_id}/artifacts/{artifact_id}"),
            "future_content_path": format!("/runner/jobs/{job_id}/artifacts/{artifact_id}/content"),
            "hint": "Reverse broker artifacts currently retain metadata only. Use runner-side artifact commands for bytes until broker content proxying lands."
        }
    }))
}

fn reconcile(job_store: &JobStore) -> Result<Value> {
    let now_ms = chrono::Utc::now().timestamp_millis().max(0) as u64;
    let reconciled = job_store.reconcile_expired_remote_runner_claims(now_ms)?;
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

fn enqueue(body: Option<Value>, job_store: &JobStore, auth: &BrokerAuthContext) -> Result<Value> {
    auth.authorize(BrokerScope::Submit, None)?;
    let request: RemoteRunnerJobRequest = parse_body(body, "remote runner job request")?;
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

fn claim(body: Option<Value>, job_store: &JobStore, auth: &BrokerAuthContext) -> Result<Value> {
    let request: ClaimRequest = parse_body(body, "remote runner claim request")?;
    auth.authorize(BrokerScope::Work, Some(request.runner_id.as_str()))?;
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
                    "Use /runner/jobs/<job-id>/events, /runner/jobs/<job-id>/finish, /runner/jobs/<job-id>/heartbeat, or /runner/jobs/<job-id>/cancel.".to_string(),
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

fn job_artifact_path(path: &str) -> Option<(Uuid, String)> {
    let tail = path.strip_prefix("/runner/jobs/")?;
    let (job_id, tail) = tail.split_once('/')?;
    let artifact_id = tail.strip_prefix("artifacts/")?;
    if artifact_id.is_empty() || artifact_id.contains('/') {
        return None;
    }
    let job_id = Uuid::parse_str(job_id).ok()?;
    Some((
        job_id,
        crate::core::execution_contract::decode_uri_component(artifact_id),
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

fn cancel(job_id: Uuid, job_store: &JobStore, auth: &BrokerAuthContext) -> Result<Value> {
    auth.authorize(BrokerScope::Submit, None)?;
    let job = job_store.cancel_remote_runner_job(job_id, "cancel requested via broker API")?;
    let events = job_store.events(job_id)?;
    Ok(json!({
        "command": "api.runner.jobs.cancel",
        "job": job,
        "events": events,
    }))
}

/// Map a handler error to an HTTP response. Broker auth rejections become
/// `401 Unauthorized` (so unauthenticated callers see a distinct status), all
/// other errors keep the existing `400 Bad Request` contract.
fn auth_or_bad_request(err: Error) -> HttpResponse {
    if err.code == crate::core::error::ErrorCode::BrokerAuthDenied {
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
    use crate::core::runner::BrokerScope;
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
