use serde_json::Value;

use super::persistence::request_metadata_string;
use super::remote_runner::RemoteRunnerJobRequest;
use super::types::{ActiveRunnerJobRunSummary, ActiveRunnerJobSummary, Job, JobStatus};
use crate::core::redaction::redact_argv_display;

pub(super) fn active_runner_job_summary(
    job: &Job,
    request: &RemoteRunnerJobRequest,
    now_ms: u64,
) -> ActiveRunnerJobSummary {
    let started_at_ms = job.started_at_ms.unwrap_or(job.created_at_ms);
    let lifecycle = request.lifecycle.clone();
    ActiveRunnerJobSummary {
        runner_id: request.runner_id.clone(),
        job_id: job.id.to_string(),
        operation: job.operation.clone(),
        source: lifecycle
            .as_ref()
            .and_then(|lifecycle| lifecycle.source.clone())
            .or_else(|| request_metadata_string(request, "source"))
            .unwrap_or_else(|| "runner-daemon".to_string()),
        kind: lifecycle
            .as_ref()
            .and_then(|lifecycle| lifecycle.kind.clone())
            .or_else(|| request_metadata_string(request, "kind"))
            .unwrap_or_else(|| job.operation.clone()),
        status: job.status,
        command: redact_argv_display(&request.command),
        cwd: request.cwd.clone(),
        started_at_ms,
        updated_at_ms: job.updated_at_ms,
        elapsed_ms: now_ms.saturating_sub(started_at_ms),
        heartbeat_age_ms: now_ms.saturating_sub(job.updated_at_ms),
        claim: super::types::JobClaimMetadata {
            claim_id: job.claim_id.clone(),
            claimed_by_runner_id: job.claimed_by_runner_id.clone(),
            claimed_at_ms: job.claimed_at_ms,
            claim_expires_at_ms: job.claim_expires_at_ms,
        },
        claim_expires_in_ms: job
            .claim_expires_at_ms
            .map(|expires_at| expires_at.saturating_sub(now_ms)),
        lifecycle: lifecycle.clone(),
        durable_run_id: lifecycle
            .as_ref()
            .and_then(|lifecycle| lifecycle.durable_run_id.clone())
            .or_else(|| request_metadata_string(request, "durable_run_id"))
            .or_else(|| request_metadata_string(request, "run_id"))
            .or_else(|| request_metadata_string(request, "record_run_id")),
        stale_reason: job.stale_reason.clone(),
        lifecycle_state: Some(runner_job_lifecycle_state(job).to_string()),
        retryable: Some(runner_job_retryable(job)),
        active_child_count: lifecycle
            .as_ref()
            .and_then(|lifecycle| lifecycle.active_child_count)
            .or_else(|| request_metadata_u64(request, "active_child_count")),
        active_cell_count: lifecycle
            .as_ref()
            .and_then(|lifecycle| lifecycle.active_cell_count)
            .or_else(|| request_metadata_u64(request, "active_cell_count")),
    }
}

fn request_metadata_u64(request: &RemoteRunnerJobRequest, key: &str) -> Option<u64> {
    request
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.get(key))
        .and_then(Value::as_u64)
}

pub fn active_runner_job_run_summary(job: ActiveRunnerJobSummary) -> ActiveRunnerJobRunSummary {
    let active_child_count = optional_count(job.active_child_count);
    let active_cell_count = optional_count(job.active_cell_count);
    let durable_run_id = job.durable_run_id.as_deref().unwrap_or("unknown");
    let command = format!(
        "{} [source={}, kind={}, runner={}, job={}, durable_run={}, elapsed_ms={}, active_child_count={}, active_cell_count={}]",
        job.command,
        job.source,
        job.kind,
        job.runner_id,
        job.job_id,
        durable_run_id,
        job.elapsed_ms,
        active_child_count,
        active_cell_count
    );
    let status_note = format!(
        "active runner job: source={} kind={} runner={} job={} durable_run={} elapsed_ms={} active_child_count={} active_cell_count={}",
        job.source,
        job.kind,
        job.runner_id,
        job.job_id,
        durable_run_id,
        job.elapsed_ms,
        active_child_count,
        active_cell_count
    );

    ActiveRunnerJobRunSummary {
        id: job
            .durable_run_id
            .clone()
            .unwrap_or_else(|| format!("runner-job-{}", job.job_id)),
        kind: job.kind,
        status: job.status.run_status_label().to_string(),
        started_at: ms_to_rfc3339(job.started_at_ms),
        command,
        cwd: job.cwd,
        status_note,
    }
}

fn optional_count(count: Option<u64>) -> String {
    count
        .map(|count| count.to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

fn ms_to_rfc3339(ms: u64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms as i64)
        .unwrap_or_else(chrono::Utc::now)
        .to_rfc3339()
}

fn runner_job_lifecycle_state(job: &Job) -> &'static str {
    if job.status == JobStatus::Failed
        && job.stale_reason.as_deref()
            == Some("daemon restarted before the job reached a terminal status")
    {
        "abandoned_after_daemon_restart"
    } else if job.stale_reason.is_some() {
        "stale"
    } else if matches!(job.status, JobStatus::Queued | JobStatus::Running) {
        "active"
    } else {
        "terminal"
    }
}

fn runner_job_retryable(job: &Job) -> bool {
    matches!(
        runner_job_lifecycle_state(job),
        "abandoned_after_daemon_restart" | "stale"
    )
}
