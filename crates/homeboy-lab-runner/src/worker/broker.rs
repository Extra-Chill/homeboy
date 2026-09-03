use std::sync::{
    mpsc::{self, RecvTimeoutError, Sender},
    Arc, Mutex,
};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use reqwest::blocking::Client;
use serde_json::json;

use homeboy_core::api_jobs::{Job, JobStatus, RemoteRunnerJobResult};
use homeboy_core::error::{Error, Result};
use homeboy_runner_contract::{
    RunnerApiClaimOutcome, RunnerApiClaimRequest, RunnerApiClaimResponse,
    RunnerApiClaimedExecution, RunnerJobExecutionProtocol, WorkspaceClaimBinding,
    WorkspaceClaimProtocol, WorkspaceOwnerLease, WorkspaceOwnerLeaseProtocol,
    RUNNER_API_CLAIM_REQUEST_SCHEMA, RUNNER_API_V1,
};

use super::super::broker_http;
use super::types::ReverseRunnerWorkerOptions;

pub(super) fn claim_job(
    client: &Client,
    options: &ReverseRunnerWorkerOptions,
) -> Result<Option<RunnerApiClaimedExecution>> {
    let data = broker_http::post_json(
        client,
        &options.broker_url,
        "/runner/jobs/claim",
        serde_json::to_value(RunnerApiClaimRequest {
            schema: RUNNER_API_CLAIM_REQUEST_SCHEMA.to_string(),
            api_version: RUNNER_API_V1,
            runner_id: options.runner_id.clone(),
            project_id: options.project_id.clone(),
            lease_ms: Some(options.lease_ms.max(1)),
            concurrency_limit: options.concurrency_limit,
            execution_protocol: Some(RunnerJobExecutionProtocol::current()),
            workspace_claim_protocol: Some(WorkspaceClaimProtocol::current()),
            workspace_owner_lease_protocol: Some(WorkspaceOwnerLeaseProtocol::current()),
        })
        .expect("runner API claim request serializes"),
        "claim reverse runner job",
        options.broker_token.as_deref(),
    )?;
    let response: RunnerApiClaimResponse = serde_json::from_value(data["response"].clone())
        .map_err(|err| {
            Error::internal_json(
                err.to_string(),
                Some("parse reverse runner job claim".to_string()),
            )
        })?;
    match response.outcome {
        RunnerApiClaimOutcome::Claimed { claim } => Ok(Some(claim)),
        RunnerApiClaimOutcome::Empty => Ok(None),
        RunnerApiClaimOutcome::Rejected { failure } => Err(Error::validation_invalid_argument(
            "claim",
            failure.message,
            None,
            None,
        )),
    }
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
    claim: &RunnerApiClaimedExecution,
    data: serde_json::Value,
    workspace_claim_binding: Option<&WorkspaceClaimBinding>,
    workspace_owner_lease: Option<&WorkspaceOwnerLease>,
) -> Result<()> {
    let claim_id = remote_runner_claim_id(claim);
    broker_http::post_json(
        client,
        broker_url,
        &format!("/runner/jobs/{}/events", claim.job_id),
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
    claim: &RunnerApiClaimedExecution,
    result: RemoteRunnerJobResult,
    workspace_claim_binding: Option<&WorkspaceClaimBinding>,
    workspace_owner_lease: Option<&WorkspaceOwnerLease>,
) -> Result<Job> {
    let claim_id = remote_runner_claim_id(claim);
    let data = broker_http::post_json(
        client,
        broker_url,
        &format!("/runner/jobs/{}/finish", claim.job_id),
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
    claim: &RunnerApiClaimedExecution,
    context_id: &str,
    workspace_claim_binding: Option<&WorkspaceClaimBinding>,
    workspace_owner_lease: Option<&WorkspaceOwnerLease>,
) -> Result<Job> {
    let claim_id = remote_runner_claim_id(claim);
    let data = broker_http::post_json(
        client,
        broker_url,
        &format!("/runner/jobs/{}/consume", claim.job_id),
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
    claim: &RunnerApiClaimedExecution,
    workspace_owner_lease: &WorkspaceOwnerLease,
) -> Result<()> {
    let claim_id = remote_runner_claim_id(claim);
    let data = broker_http::post_json(
        client,
        broker_url,
        &format!("/runner/jobs/{}/validate-owner", claim.job_id),
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
            Some(claim.job_id.clone()),
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
    claim: &RunnerApiClaimedExecution,
    lease_ms: u64,
    workspace_claim_binding: Option<&WorkspaceClaimBinding>,
    workspace_owner_lease: Option<&WorkspaceOwnerLease>,
) -> Result<(Job, Option<WorkspaceOwnerLease>)> {
    let claim_id = remote_runner_claim_id(claim);
    let data = broker_http::post_json(
        client,
        broker_url,
        &format!("/runner/jobs/{}/heartbeat", claim.job_id),
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
    claim: &RunnerApiClaimedExecution,
    workspace_claim_binding: Option<WorkspaceClaimBinding>,
    workspace_owner_lease: Option<WorkspaceOwnerLease>,
) -> Result<ClaimHeartbeat> {
    remote_runner_claim_id(claim);
    let (stop, stopped) = mpsc::channel();
    let client = client.clone();
    let broker_url = options.broker_url.clone();
    let broker_token = options.broker_token.clone();
    let runner_id = options.runner_id.clone();
    let claim = claim.clone();
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
            &claim,
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
                        "job_id": claim.job_id,
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
    claim: &RunnerApiClaimedExecution,
) -> Result<Option<Job>> {
    let data = broker_http::get_json(
        client,
        broker_url,
        &format!("/jobs/{}", claim.job_id),
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

pub(super) fn remote_runner_claim_id(claim: &RunnerApiClaimedExecution) -> &str {
    &claim.claim_id
}
