//! Runner-side implementation of core's `RunnerContinuationProvider` hook.
//!
//! Core's `agent_task_lifecycle` calls this contract to reconcile and resume a
//! run that was dispatched to a remote runner, without depending on runner
//! behavior directly. This adapter delegates to the runner connection,
//! execution, and evidence functions.

use homeboy_agents::agent_task_lifecycle::{RunnerContinuationProvider, RunnerJobReconciliation};
use homeboy_core::api_jobs::{Job, RemoteRunnerJobRequest, RunnerJobLogSnapshot};
use std::time::Duration;

use reqwest::blocking::Client;
use serde::Deserialize;
use serde_json::{json, Value};

use homeboy_core::error::{Error, Result};
use homeboy_core::workspace_claim::{
    WorkspaceAuthorityStatus, WorkspaceClaim, WorkspaceClaimProtocol, WorkspaceIdentity,
    WORKSPACE_CLAIM_CAPABILITY,
};

/// The runner layer's `RunnerContinuationProvider`. Registered with core at startup.
pub struct RunnerContinuation;

impl RunnerContinuationProvider for RunnerContinuation {
    fn supports_workspace_claims(&self) -> bool {
        true
    }

    fn supports_terminal_workspace_authority(&self) -> bool {
        true
    }

    fn workspace_claim_runner_ids(&self) -> Result<Vec<String>> {
        Ok(super::list()?
            .into_iter()
            .filter(|runner| runner.id != "local")
            .map(|runner| runner.id)
            .collect())
    }

    fn acquire_workspace_claim(
        &self,
        runner_id: &str,
        workspace: WorkspaceIdentity,
        lifecycle_revision: u64,
    ) -> Result<WorkspaceClaim> {
        let body = workspace_claim_post(
            runner_id,
            "/workspace-claims/acquire",
            json!({
                "workspace": workspace,
                "lifecycle_revision": lifecycle_revision,
                "ttl_ms": homeboy_core::workspace_claim::MAX_WORKSPACE_CLAIM_TTL_MS,
            }),
        )?;
        let claim: WorkspaceClaim = serde_json::from_value(
            body.get("claim").cloned().unwrap_or(Value::Null),
        )
        .map_err(|error| {
            workspace_claim_error(runner_id, format!("malformed acquire response: {error}"))
        })?;
        claim.protocol.verify()?;
        if claim.workspace != workspace {
            return Err(workspace_claim_error(
                runner_id,
                "daemon returned a claim for a different workspace",
            ));
        }
        Ok(claim)
    }

    fn validate_workspace_claim(&self, runner_id: &str, claim: &WorkspaceClaim) -> Result<bool> {
        let body = workspace_claim_post(
            runner_id,
            "/workspace-claims/validate",
            json!({ "claim": claim }),
        )?;
        body.get("valid").and_then(Value::as_bool).ok_or_else(|| {
            workspace_claim_error(
                runner_id,
                "malformed validate response missing boolean valid",
            )
        })
    }

    fn release_workspace_claim(&self, runner_id: &str, claim: &WorkspaceClaim) -> Result<()> {
        let body = workspace_claim_post(
            runner_id,
            "/workspace-claims/release",
            json!({ "claim": claim }),
        )?;
        if body.get("released").and_then(Value::as_bool) != Some(true) {
            return Err(workspace_claim_error(
                runner_id,
                "malformed release response missing released acknowledgement",
            ));
        }
        Ok(())
    }

    fn workspace_claim_authority_is_clear(
        &self,
        runner_id: &str,
        workspace: &WorkspaceIdentity,
    ) -> Result<bool> {
        // This uses the same deterministic transport selection as acquisition:
        // direct sessions inspect the daemon and reverse sessions inspect the
        // broker, which are the respective stores that can own this workspace.
        let body = workspace_claim_post(
            runner_id,
            "/workspace-claims/authority",
            json!({ "workspace": workspace }),
        )?;
        let status: WorkspaceAuthorityStatus = serde_json::from_value(
            body.get("status").cloned().unwrap_or(Value::Null),
        )
        .map_err(|error| {
            workspace_claim_error(runner_id, format!("malformed authority response: {error}"))
        })?;
        status.verify(workspace)?;
        Ok(status.clear)
    }
    fn runner_job_log_snapshot(
        &self,
        runner_id: &str,
        job_id: &str,
    ) -> Result<RunnerJobLogSnapshot> {
        super::evidence::runner_job_log_snapshot(runner_id, job_id)
    }

    fn reconcile_runner_job(&self, runner_id: &str, job_id: &str) -> RunnerJobReconciliation {
        match self.runner_job_log_snapshot(runner_id, job_id) {
            Ok(snapshot) => return RunnerJobReconciliation::Snapshot(snapshot),
            Err(error) if !job_not_found(&error, job_id) => {
                return RunnerJobReconciliation::UnconfirmedAbsence;
            }
            Err(_) => {}
        }

        let Ok(status) = super::connection::status(runner_id) else {
            return RunnerJobReconciliation::UnconfirmedAbsence;
        };
        let Some(session) = status.session.filter(|_| status.connected) else {
            return RunnerJobReconciliation::UnconfirmedAbsence;
        };
        let Ok(generations) = super::generation_store::live_sessions(runner_id, Some(&session))
        else {
            return RunnerJobReconciliation::UnconfirmedAbsence;
        };
        if generations.is_empty() {
            return RunnerJobReconciliation::UnconfirmedAbsence;
        }

        let mut checked_generations = 0;
        for generation in generations {
            if generation.local_url.is_none() {
                return RunnerJobReconciliation::UnconfirmedAbsence;
            }
            checked_generations += 1;
            match super::evidence::runner_job_log_snapshot_for_session(&generation, job_id) {
                Ok(snapshot) => {
                    if super::generation_store::record_job(runner_id, &generation, job_id).is_err()
                    {
                        return RunnerJobReconciliation::UnconfirmedAbsence;
                    }
                    return RunnerJobReconciliation::Snapshot(snapshot);
                }
                Err(error) if job_not_found(&error, job_id) => continue,
                Err(_) => return RunnerJobReconciliation::UnconfirmedAbsence,
            }
        }
        RunnerJobReconciliation::ConfirmedAbsent {
            checked_generations,
        }
    }

    fn is_runner_connected(&self, runner_id: &str) -> bool {
        // Preserve the original lifecycle semantics: only an affirmative
        // `connected == false` should be treated as disconnected. A status
        // *error* (transient lookup failure) must NOT annotate the run as
        // disconnected, so assume connected when the status can't be read.
        super::connection::status(runner_id)
            .map(|report| report.connected)
            .unwrap_or(true)
    }

    fn runner_exists(&self, runner_id: &str) -> bool {
        super::exists(runner_id)
    }

    fn run_continuation_exec(
        &self,
        runner_id: &str,
        cwd: &str,
        command: &[String],
        run_id: &str,
    ) -> Result<i32> {
        let (_, exit_code) = super::execution::exec(
            runner_id,
            super::execution::RunnerExecOptions {
                cwd: Some(cwd.to_string()),
                command: command.to_vec(),
                run_id: Some(run_id.to_string()),
                ..Default::default()
            },
        )?;
        Ok(exit_code)
    }

    fn submit_reverse_broker_job(
        &self,
        runner_id: &str,
        request: RemoteRunnerJobRequest,
    ) -> Result<Job> {
        super::connection::submit_reverse_broker_job(runner_id, request)
    }

    fn lookup_reverse_broker_submission(
        &self,
        runner_id: &str,
        submission_key: &str,
    ) -> Result<homeboy_core::api_jobs::RemoteRunnerSubmissionLookup> {
        super::connection::lookup_reverse_broker_submission(runner_id, submission_key)
    }
}

#[derive(Deserialize)]
struct DaemonEnvelope {
    success: bool,
    data: Option<Value>,
}

fn workspace_claim_post(runner_id: &str, path: &str, payload: Value) -> Result<Value> {
    let report = super::connection::status(runner_id)?;
    let session = report
        .session
        .filter(|_| report.connected)
        .ok_or_else(|| workspace_claim_error(runner_id, "direct daemon is not connected"))?;
    if session.mode == super::RunnerTunnelMode::Reverse {
        let broker_url = session.broker_url.ok_or_else(|| {
            workspace_claim_error(runner_id, "reverse runner session has no broker endpoint")
        })?;
        return reverse_workspace_claim_post(runner_id, &broker_url, path, payload);
    }
    if session.mode != super::RunnerTunnelMode::DirectSsh {
        return Err(workspace_claim_error(
            runner_id,
            "workspace claims require a direct daemon transport",
        ));
    }
    let local_url = session.local_url.ok_or_else(|| {
        workspace_claim_error(runner_id, "direct daemon session has no local endpoint")
    })?;
    let client = Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|error| {
            workspace_claim_error(runner_id, format!("build daemon client: {error}"))
        })?;
    let capabilities = daemon_get_json(&client, &local_url, "/capabilities", runner_id)?;
    let protocols: Vec<WorkspaceClaimProtocol> = serde_json::from_value(
        capabilities
            .get("capabilities")
            .cloned()
            .unwrap_or(Value::Null),
    )
    .map_err(|error| {
        workspace_claim_error(runner_id, format!("malformed daemon capabilities: {error}"))
    })?;
    if !protocols.iter().any(|protocol| {
        protocol.capability == WORKSPACE_CLAIM_CAPABILITY && protocol.verify().is_ok()
    }) {
        return Err(workspace_claim_error(
            runner_id,
            "daemon does not advertise workspace claim capability v1",
        ));
    }
    daemon_post_json(&client, &local_url, path, payload, runner_id)
}

fn reverse_workspace_claim_post(
    runner_id: &str,
    broker_url: &str,
    daemon_path: &str,
    payload: Value,
) -> Result<Value> {
    let client = Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|error| {
            workspace_claim_error(runner_id, format!("build broker client: {error}"))
        })?;
    let token = homeboy_core::broker_auth::broker_submit_token_for_runner(runner_id)?;
    let capabilities = client
        .get(format!(
            "{}/runner/workspace-claims/capabilities",
            broker_url.trim_end_matches('/')
        ))
        .bearer_auth(token.as_deref().unwrap_or_default())
        .send()
        .map_err(|error| {
            workspace_claim_error(
                runner_id,
                format!("broker capability request failed: {error}"),
            )
        })?;
    let capabilities = daemon_response(
        capabilities.status().as_u16(),
        capabilities.text(),
        "/runner/workspace-claims/capabilities",
        runner_id,
    )?;
    let protocols: Vec<WorkspaceClaimProtocol> = serde_json::from_value(
        capabilities
            .get("capabilities")
            .cloned()
            .unwrap_or(Value::Null),
    )
    .map_err(|error| {
        workspace_claim_error(runner_id, format!("malformed broker capabilities: {error}"))
    })?;
    if !protocols.iter().any(|protocol| {
        protocol.capability == WORKSPACE_CLAIM_CAPABILITY && protocol.verify().is_ok()
    }) {
        return Err(workspace_claim_error(
            runner_id,
            "reverse broker does not advertise workspace claim capability v1",
        ));
    }
    let operation = daemon_path.trim_start_matches("/workspace-claims/");
    let response = client
        .post(format!(
            "{}/runner/workspace-claims/{operation}",
            broker_url.trim_end_matches('/')
        ))
        .bearer_auth(token.as_deref().unwrap_or_default())
        .json(&payload)
        .send()
        .map_err(|error| {
            workspace_claim_error(runner_id, format!("broker claim request failed: {error}"))
        })?;
    daemon_response(
        response.status().as_u16(),
        response.text(),
        "/runner/workspace-claims",
        runner_id,
    )
}

fn daemon_get_json(client: &Client, url: &str, path: &str, runner_id: &str) -> Result<Value> {
    let response = client
        .get(format!("{}{}", url.trim_end_matches('/'), path))
        .send()
        .map_err(|error| {
            workspace_claim_error(
                runner_id,
                format!("daemon capability request failed: {error}"),
            )
        })?;
    daemon_response(response.status().as_u16(), response.text(), path, runner_id)
}

fn daemon_post_json(
    client: &Client,
    url: &str,
    path: &str,
    payload: Value,
    runner_id: &str,
) -> Result<Value> {
    let response = client
        .post(format!("{}{}", url.trim_end_matches('/'), path))
        .json(&payload)
        .send()
        .map_err(|error| {
            workspace_claim_error(runner_id, format!("daemon claim request failed: {error}"))
        })?;
    daemon_response(response.status().as_u16(), response.text(), path, runner_id)
}

fn daemon_response(
    status: u16,
    body: reqwest::Result<String>,
    path: &str,
    runner_id: &str,
) -> Result<Value> {
    let body = body.map_err(|error| {
        workspace_claim_error(runner_id, format!("read daemon {path} response: {error}"))
    })?;
    let envelope: DaemonEnvelope = serde_json::from_str(&body).map_err(|error| {
        workspace_claim_error(
            runner_id,
            format!("malformed daemon {path} response: {error}"),
        )
    })?;
    if status != 200 || !envelope.success {
        return Err(workspace_claim_error(
            runner_id,
            format!("daemon {path} refused workspace claim request"),
        ));
    }
    let data = envelope.data.ok_or_else(|| {
        workspace_claim_error(runner_id, format!("daemon {path} response has no data"))
    })?;
    data.get("body").cloned().ok_or_else(|| {
        workspace_claim_error(
            runner_id,
            format!("daemon {path} response has no canonical body"),
        )
    })
}

fn workspace_claim_error(runner_id: &str, message: impl Into<String>) -> Error {
    Error::validation_invalid_argument(
        "workspace_claim",
        message,
        Some(runner_id.to_string()),
        None,
    )
}

fn job_not_found(error: &homeboy_core::error::Error, job_id: &str) -> bool {
    error
        .details
        .get("http_status")
        .and_then(serde_json::Value::as_u64)
        == Some(404)
        && error
            .details
            .get("path")
            .and_then(serde_json::Value::as_str)
            == Some(&format!("/jobs/{job_id}"))
}

/// Register the runner continuation provider with core. Called once at startup.
pub fn register() {
    homeboy_agents::agent_task_lifecycle::register_runner_continuation_provider(Box::new(
        RunnerContinuation,
    ));
}
