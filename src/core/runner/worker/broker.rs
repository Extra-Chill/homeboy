use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use reqwest::blocking::Client;
use serde_json::json;

use crate::core::api_jobs::{Job, JobStatus, RemoteRunnerJobClaim, RemoteRunnerJobResult};
use crate::core::error::{Error, Result};

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

pub(super) fn append_progress(
    client: &Client,
    broker_url: &str,
    token: Option<&str>,
    runner_id: &str,
    job: &Job,
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
            "message": "reverse runner worker started execution",
        }),
        "append reverse runner progress event",
        token,
    )?;
    Ok(())
}

pub(super) fn finish_job(
    client: &Client,
    broker_url: &str,
    token: Option<&str>,
    runner_id: &str,
    job: &Job,
    result: RemoteRunnerJobResult,
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
        }),
        "finish reverse runner job",
        token,
    )?;
    serde_json::from_value(data["job"].clone()).map_err(|err| {
        Error::internal_json(
            err.to_string(),
            Some("parse finished reverse runner job".to_string()),
        )
    })
}

fn renew_claim(
    client: &Client,
    broker_url: &str,
    token: Option<&str>,
    runner_id: &str,
    job: &Job,
    lease_ms: u64,
) -> Result<Job> {
    let claim_id = remote_runner_claim_id(job)?;
    let data = broker_http::post_json(
        client,
        broker_url,
        &format!("/runner/jobs/{}/heartbeat", job.id),
        json!({
            "runner_id": runner_id,
            "claim_id": claim_id,
            "lease_ms": lease_ms.max(1),
        }),
        "renew reverse runner claim",
        token,
    )?;
    serde_json::from_value(data["job"].clone()).map_err(|err| {
        Error::internal_json(
            err.to_string(),
            Some("parse reverse runner heartbeat job".to_string()),
        )
    })
}

pub(super) fn start_claim_heartbeat(
    client: &Client,
    options: &ReverseRunnerWorkerOptions,
    job: &Job,
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
    let handle = thread::spawn(move || loop {
        match stopped.recv_timeout(interval) {
            Ok(()) | Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => {}
        }
        if let Err(err) = renew_claim(
            &client,
            &broker_url,
            broker_token.as_deref(),
            &runner_id,
            &job,
            lease_ms,
        ) {
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
    });
    Ok(ClaimHeartbeat {
        stop: Some(stop),
        handle: Some(handle),
    })
}

pub(super) struct ClaimHeartbeat {
    stop: Option<Sender<()>>,
    handle: Option<JoinHandle<()>>,
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
