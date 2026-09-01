use std::sync::{
    mpsc::{self, RecvTimeoutError, Sender},
    Arc, Mutex,
};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use reqwest::blocking::Client;
use serde_json::json;

use homeboy_core::api_jobs::{Job, JobStatus, RemoteRunnerJobClaim, RemoteRunnerJobResult};
use homeboy_core::error::{Error, Result};
use homeboy_runner_contract::{
    WorkspaceClaimBinding, WorkspaceClaimProtocol, WorkspaceOwnerLease, WorkspaceOwnerLeaseProtocol,
};

use super::super::broker_http;
use super::types::ReverseRunnerWorkerOptions;

pub(super) fn claim_job(
    client: &Client,
    options: &ReverseRunnerWorkerOptions,
) -> Result<Option<RemoteRunnerJobClaim>> {
    let data = broker_http::post_json(
        client,
        &options.broker_url,
        "/runner/jobs/claim",
        json!({
            "runner_id": options.runner_id,
            "project_id": options.project_id,
            "lease_ms": options.lease_ms.max(1),
            "concurrency_limit": options.concurrency_limit,
            "execution_protocol": homeboy_core::runner_job_execution_context::RunnerJobExecutionProtocol::current(),
            "workspace_claim_protocol": WorkspaceClaimProtocol::current(),
            "workspace_owner_lease_protocol": WorkspaceOwnerLeaseProtocol::current(),
        }),
        "claim reverse runner job",
        options.broker_token.as_deref(),
    )?;
    let claim = data["claim"].clone();
    if claim.is_null() {
        return Ok(None);
    }
    serde_json::from_value(claim).map(Some).map_err(|err| {
        Error::internal_json(
            err.to_string(),
            Some("parse reverse runner job claim".to_string()),
        )
    })
}

#[expect(
    clippy::too_many_arguments,
    reason = "Progress must carry the exact claim and workspace lease bindings that authorize the event."
)]
pub(super) fn append_progress_data(
    client: &Client,
    broker_url: &str,
    token: Option<&str>,
    runner_id: &str,
    job: &Job,
    data: serde_json::Value,
    workspace_claim_binding: Option<&WorkspaceClaimBinding>,
    workspace_owner_lease: Option<&WorkspaceOwnerLease>,
) -> Result<()> {
    let claim_id = remote_runner_claim_id(job)?;
    broker_http::post_json(
        client,
        broker_url,
        &format!("/runner/jobs/{}/events", job.id),
        json!({
            "runner_id": runner_id,
            "claim_id": claim_id,
            "kind": "progress",
            "data": data,
            "workspace_claim_binding": workspace_claim_binding,
            "workspace_owner_lease": workspace_owner_lease,
        }),
        "append reverse runner progress event",
        token,
    )?;
    Ok(())
}

#[expect(
    clippy::too_many_arguments,
    reason = "Terminal result submission preserves independently verified claim and workspace lease bindings."
)]
pub(super) fn finish_job(
    client: &Client,
    broker_url: &str,
    token: Option<&str>,
    runner_id: &str,
    job: &Job,
    result: RemoteRunnerJobResult,
    workspace_claim_binding: Option<&WorkspaceClaimBinding>,
    workspace_owner_lease: Option<&WorkspaceOwnerLease>,
) -> Result<Job> {
    let claim_id = remote_runner_claim_id(job)?;
    let data = broker_http::post_json(
        client,
        broker_url,
        &format!("/runner/jobs/{}/finish", job.id),
        json!({
            "runner_id": runner_id,
            "claim_id": claim_id,
            "result": result,
            "workspace_claim_binding": workspace_claim_binding,
            "workspace_owner_lease": workspace_owner_lease,
        }),
        "finish reverse runner job",
        token,
    )?;
    let job = serde_json::from_value(data["job"].clone()).map_err(|err| {
        Error::internal_json(
            err.to_string(),
            Some("parse finished reverse runner job".to_string()),
        )
    })?;
    Ok(job)
}

#[expect(
    clippy::too_many_arguments,
    reason = "Receipt consumption must keep its context, claim, and workspace lease evidence distinct."
)]
pub(super) fn consume_execution(
    client: &Client,
    broker_url: &str,
    token: Option<&str>,
    runner_id: &str,
    job: &Job,
    context_id: &str,
    workspace_claim_binding: Option<&WorkspaceClaimBinding>,
    workspace_owner_lease: Option<&WorkspaceOwnerLease>,
) -> Result<Job> {
    let claim_id = remote_runner_claim_id(job)?;
    let data = broker_http::post_json(
        client,
        broker_url,
        &format!("/runner/jobs/{}/consume", job.id),
        json!({
            "runner_id": runner_id,
            "claim_id": claim_id,
            "context_id": context_id,
            "workspace_claim_binding": workspace_claim_binding,
            "workspace_owner_lease": workspace_owner_lease,
        }),
        "consume reverse runner execution receipt",
        token,
    )?;
    let job = serde_json::from_value(data["job"].clone()).map_err(|err| {
        Error::internal_json(
            err.to_string(),
            Some("parse consumed reverse runner job".to_string()),
        )
    })?;
    Ok(job)
}

/// Check the exact durable owner lease without consuming the execution receipt.
/// Input materialization is intentionally gated on this call, not on consume.
pub(super) fn validate_workspace_owner(
    client: &Client,
    broker_url: &str,
    token: Option<&str>,
    runner_id: &str,
    job: &Job,
    workspace_owner_lease: &WorkspaceOwnerLease,
) -> Result<()> {
    let claim_id = remote_runner_claim_id(job)?;
    let data = broker_http::post_json(
        client,
        broker_url,
        &format!("/runner/jobs/{}/validate-owner", job.id),
        json!({
            "runner_id": runner_id,
            "claim_id": claim_id,
            "workspace_owner_lease": workspace_owner_lease,
        }),
        "validate reverse runner workspace owner lease",
        token,
    )?;
    if data["valid"].as_bool() != Some(true) {
        return Err(Error::validation_invalid_argument(
            "workspace_owner_lease",
            "reverse runner workspace owner lease is no longer live",
            Some(job.id.to_string()),
            None,
        ));
    }
    Ok(())
}

#[expect(
    clippy::too_many_arguments,
    reason = "Lease renewal carries every current authorization binding to prevent stale-owner renewal."
)]
fn renew_claim(
    client: &Client,
    broker_url: &str,
    token: Option<&str>,
    runner_id: &str,
    job: &Job,
    lease_ms: u64,
    workspace_claim_binding: Option<&WorkspaceClaimBinding>,
    workspace_owner_lease: Option<&WorkspaceOwnerLease>,
) -> Result<(Job, Option<WorkspaceOwnerLease>)> {
    let claim_id = remote_runner_claim_id(job)?;
    let data = broker_http::post_json(
        client,
        broker_url,
        &format!("/runner/jobs/{}/heartbeat", job.id),
        json!({
            "runner_id": runner_id,
            "claim_id": claim_id,
            "lease_ms": lease_ms.max(1),
            "workspace_claim_binding": workspace_claim_binding,
            "workspace_owner_lease": workspace_owner_lease,
        }),
        "renew reverse runner claim",
        token,
    )?;
    let job = serde_json::from_value(data["job"].clone()).map_err(|err| {
        Error::internal_json(
            err.to_string(),
            Some("parse reverse runner heartbeat job".to_string()),
        )
    })?;
    let owner = serde_json::from_value(data["workspace_owner_lease"].clone()).ok();
    Ok((job, owner))
}

pub(super) fn start_claim_heartbeat(
    client: &Client,
    options: &ReverseRunnerWorkerOptions,
    job: &Job,
    workspace_claim_binding: Option<WorkspaceClaimBinding>,
    workspace_owner_lease: Option<WorkspaceOwnerLease>,
) -> Result<ClaimHeartbeat> {
    remote_runner_claim_id(job)?;
    let (stop, stopped) = mpsc::channel();
    let client = client.clone();
    let broker_url = options.broker_url.clone();
    let broker_token = options.broker_token.clone();
    let runner_id = options.runner_id.clone();
    let job = job.clone();
    let lease_ms = options.lease_ms.max(1);
    let interval = Duration::from_millis((lease_ms / 2).max(1));
    let current_owner = Arc::new(Mutex::new(workspace_owner_lease));
    let heartbeat_owner = current_owner.clone();
    let handle = thread::spawn(move || loop {
        match stopped.recv_timeout(interval) {
            Ok(()) | Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => {}
        }
        let presented_owner = heartbeat_owner.lock().expect("owner lease lock").clone();
        match renew_claim(
            &client,
            &broker_url,
            broker_token.as_deref(),
            &runner_id,
            &job,
            lease_ms,
            workspace_claim_binding.as_ref(),
            presented_owner.as_ref(),
        ) {
            Ok((_job, renewed_owner)) => {
                *heartbeat_owner.lock().expect("owner lease lock") = renewed_owner
            }
            Err(err) => {
                eprintln!(
                    "{}",
                    json!({
                        "command": "runner.work",
                        "event": "claim_heartbeat_failed",
                        "runner_id": runner_id,
                        "broker_url": broker_url,
                        "job_id": job.id,
                        "error": err.to_string(),
                    })
                );
            }
        }
    });
    Ok(ClaimHeartbeat {
        stop: Some(stop),
        handle: Some(handle),
        owner_lease: current_owner,
    })
}

pub(super) struct ClaimHeartbeat {
    stop: Option<Sender<()>>,
    handle: Option<JoinHandle<()>>,
    owner_lease: Arc<Mutex<Option<WorkspaceOwnerLease>>>,
}

impl ClaimHeartbeat {
    pub(super) fn owner_lease_handle(&self) -> Arc<Mutex<Option<WorkspaceOwnerLease>>> {
        self.owner_lease.clone()
    }
}

impl Drop for ClaimHeartbeat {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

pub(super) fn cancelled_job_snapshot(
    client: &Client,
    broker_url: &str,
    token: Option<&str>,
    job: &Job,
) -> Result<Option<Job>> {
    let data = broker_http::get_json(
        client,
        broker_url,
        &format!("/jobs/{}", job.id),
        "inspect reverse runner job cancellation state",
        token,
    )?;
    let snapshot: Job = serde_json::from_value(data["job"].clone()).map_err(|err| {
        Error::internal_json(
            err.to_string(),
            Some("parse reverse runner job snapshot".to_string()),
        )
    })?;
    if snapshot.status == JobStatus::Cancelled {
        Ok(Some(snapshot))
    } else {
        Ok(None)
    }
}

pub(super) fn remote_runner_claim_id(job: &Job) -> Result<&str> {
    job.claim_id.as_deref().ok_or_else(|| {
        Error::validation_invalid_argument(
            "claim_id",
            "claimed remote runner job is missing a claim id",
            Some(job.id.to_string()),
            None,
        )
    })
}
