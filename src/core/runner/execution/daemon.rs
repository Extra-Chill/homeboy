use std::collections::HashMap;
use std::time::{Duration, Instant};

use reqwest::blocking::Client;
use serde_json::{json, Value};

use crate::command_contract::RunnerWorkload;
use crate::core::api_jobs::{Job, JobEvent, JobStatus};
use crate::core::engine::command::CommandCaptureMetadata;
use crate::core::error::{Error, Result};
use crate::core::redaction::redact_argv;
use crate::core::source_snapshot::SourceSnapshot;

use super::super::capabilities::{
    runner_capability_snapshot_for_preflight, validate_runner_capability_preflight,
};
use super::super::daemon_http_get::daemon_get;
use super::super::evidence::{mirror_daemon_evidence, mirror_daemon_job_progress};
use super::super::resource_metrics::RunnerResourceMetrics;
use super::super::{Runner, RunnerCapabilityPreflight, RunnerJob, RunnerKind};

#[allow(unused_imports)]
use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) fn exec_via_daemon(
    runner: &Runner,
    local_url: &str,
    cwd: String,
    project_id: Option<String>,
    command: Vec<String>,
    env: HashMap<String, String>,
    secret_env_names: Vec<String>,
    capture_patch: bool,
    source_snapshot_override: Option<SourceSnapshot>,
    require_paths: Vec<String>,
    runner_workload: Option<RunnerWorkload>,
    detach_after_handoff: bool,
) -> Result<(RunnerExecOutput, i32)> {
    let client = Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|err| Error::internal_unexpected(format!("build daemon HTTP client: {err}")))?;
    let source_snapshot = source_snapshot_override.unwrap_or_else(|| {
        SourceSnapshot::existing_remote(&runner.id, &cwd, runner.workspace_root.as_deref())
    });
    let response = client
        .post(format!("{}/exec", local_url.trim_end_matches('/')))
        .json(&json!({
            "runner_id": runner.id,
            "runner": runner,
            "project_id": project_id,
            "cwd": cwd,
            "command": command,
            "env": env,
            "secret_env_names": secret_env_names,
            "capture_patch": capture_patch,
            "source_snapshot": source_snapshot.clone(),
            "require_paths": require_paths.clone(),
            "runner_workload": runner_workload,
        }))
        .send()
        .map_err(|err| daemon_exec_transport_error(&runner.id, err))?;
    let status_code = response.status().as_u16();
    let envelope: DaemonEnvelope = response.json().map_err(|err| {
        // A stale/restarting daemon can answer the tunnel with a non-JSON or
        // empty body. Surface a clear, actionable error instead of a bare parse
        // failure so the caller knows to reconnect (#3631, #3624).
        daemon_exec_stale_response_error(&runner.id, status_code, &err.to_string())
    })?;
    if status_code >= 400 || !envelope.success {
        return Err(daemon_exec_request_failed_error(
            &runner.id,
            status_code,
            &envelope,
        ));
    }

    let data = envelope
        .data
        .ok_or_else(|| Error::internal_unexpected("daemon exec returned no data"))?;
    let body = canonical_daemon_body(&data, "daemon exec response")?;
    let job_value = body
        .get("job")
        .ok_or_else(|| Error::internal_unexpected("daemon exec returned no job"))?;
    let mut job: Job = serde_json::from_value(job_value.clone()).map_err(|err| {
        Error::internal_json(err.to_string(), Some("parse daemon exec job".to_string()))
    })?;
    let persisted_run_id = persist_lab_offload_handoff_run(runner, &cwd, &command, &job);
    if detach_after_handoff {
        return Ok(detached_handoff_output(
            runner,
            RunnerExecMode::Daemon,
            cwd,
            command,
            source_snapshot,
            job,
            require_paths,
            persisted_run_id,
        ));
    }

    let deadline = Instant::now() + runner_exec_wait_timeout();
    while !matches!(
        job.status,
        JobStatus::Succeeded | JobStatus::Failed | JobStatus::Cancelled
    ) {
        if Instant::now() >= deadline {
            let events = fetch_daemon_events(&client, local_url, &job.id.to_string())
                .map(|events| redact_runner_job_events(&events, &env, &secret_env_names))
                .unwrap_or_default();
            return Err(daemon_job_wait_timeout(
                runner,
                &cwd,
                &command,
                &job,
                &events,
                "runner daemon job",
                true,
            ));
        }
        std::thread::sleep(Duration::from_millis(200));
        let job_id = job.id.to_string();
        job = fetch_daemon_job_resilient(&client, local_url, &job_id)
            .map_err(|err| daemon_job_context_error(&runner.id, &job_id, err))?;
    }
    let job_id = job.id.to_string();
    let events = redact_runner_job_events(
        &fetch_daemon_events(&client, local_url, &job_id)
            .map_err(|err| daemon_job_context_error(&runner.id, &job_id, err))?,
        &env,
        &secret_env_names,
    );

    let RunnerJobResultFields {
        result,
        stdout,
        stderr,
        metrics,
        capture,
        exit_code,
    } = runner_job_result_fields(&events, job.status, &env, &secret_env_names);

    let mirror = mirror_daemon_evidence(runner, &cwd, &command, &job, &events, &result)?;
    let patch = mirror.as_ref().and_then(|evidence| evidence.patch.clone());
    let mirror_run_id = mirror.as_ref().map(|evidence| evidence.run.id.clone());
    let artifacts = job.artifacts.clone();
    let mutation_artifacts = mutation_artifacts_from_job(&job, &result);
    print_lab_offload_handoff(
        &runner.id,
        Some(&cwd),
        &job.id.to_string(),
        mirror_run_id.as_deref(),
        DaemonJobHandoffState::Terminal(job.status),
    );

    let runner_job = RunnerJob::from_job(&runner.id, "daemon", &command, Some(cwd.clone()), &job);
    let runner_result = runner_result(
        Some(&job),
        exit_code,
        &stdout,
        &stderr,
        mirror_run_id.as_deref(),
        mutation_artifacts.clone(),
    );
    let handoff = runner_handoff(
        runner,
        "daemon",
        Some(runner_job.clone()),
        Some(runner_result.clone()),
    );

    Ok((
        RunnerExecOutput {
            variant: "exec",
            command: "runner.exec",
            runner_id: runner.id.clone(),
            dry_run: false,
            mode: RunnerExecMode::Daemon,
            argv: redact_argv(&command),
            remote_cwd: cwd,
            exit_code,
            stdout,
            stderr,
            source_snapshot: Some(source_snapshot.clone()),
            job_id: Some(job.id.to_string()),
            job: Some(job),
            runner_job: Some(runner_job),
            job_events: Some(events),
            mirror_run_id,
            patch,
            mutation_artifacts,
            artifacts,
            metrics,
            capture,
            runner_result: Some(runner_result),
            handoff: Some(handoff),
            diagnostics: runner_exec_diagnostics(runner, Some(&source_snapshot), &require_paths),
        },
        exit_code,
    ))
}

pub(super) fn preflight_runner_capability_plan(
    runner: &Runner,
    preflight: Option<&RunnerCapabilityPreflight>,
    request_env: &HashMap<String, String>,
) -> Result<()> {
    let Some(preflight) = preflight else {
        return Ok(());
    };
    if preflight.is_empty() || runner.kind != RunnerKind::Ssh {
        return Ok(());
    }

    let capabilities = runner_capability_snapshot_for_preflight(runner, preflight)?;
    validate_runner_capability_preflight(&runner.id, preflight, &capabilities, request_env)
}

pub(super) fn fetch_daemon_job(client: &Client, local_url: &str, job_id: &str) -> Result<Job> {
    let data = daemon_get(client, local_url, &format!("/jobs/{job_id}"))?;
    let body = canonical_daemon_body(&data, "daemon job response")?;
    serde_json::from_value(body["job"].clone())
        .map_err(|err| Error::internal_json(err.to_string(), Some("parse daemon job".to_string())))
}

pub(super) fn detached_handoff_output(
    runner: &Runner,
    mode: RunnerExecMode,
    cwd: String,
    command: Vec<String>,
    source_snapshot: SourceSnapshot,
    job: Job,
    require_paths: Vec<String>,
    mirror_run_id: Option<String>,
) -> (RunnerExecOutput, i32) {
    let job_id = job.id.to_string();
    print_lab_offload_handoff(
        &runner.id,
        Some(&cwd),
        &job_id,
        mirror_run_id.as_deref(),
        DaemonJobHandoffState::InFlight,
    );
    let stdout = serde_json::to_string_pretty(&json!({
        "schema": "homeboy/runner-exec-handoff/v1",
        "status": "handoff_complete",
        "execution_location": format!("runner:{}", runner.id),
        "runner_id": runner.id.clone(),
        "job_id": job_id,
        "persisted_run_id": mirror_run_id.as_deref(),
        "mirror_run_id": mirror_run_id.as_deref(),
        "remote_cwd": cwd.clone(),
        "follow_commands": {
            "job_logs": format!("homeboy runner job logs {} {} --follow", runner.id, job.id),
            "job_cancel": format!("homeboy runner job cancel {} {}", runner.id, job.id),
        }
    }))
    .unwrap_or_else(|_| "{}".to_string());
    let transport = match mode {
        RunnerExecMode::ReverseBroker => "reverse_broker",
        _ => "daemon",
    };
    let runner_job = RunnerJob::from_job(&runner.id, transport, &command, Some(cwd.clone()), &job);
    let runner_result = runner_result(Some(&job), 0, &stdout, "", mirror_run_id.as_deref(), None);
    let handoff = runner_handoff(
        runner,
        transport,
        Some(runner_job.clone()),
        Some(runner_result.clone()),
    );

    (
        RunnerExecOutput {
            variant: "exec",
            command: "runner.exec",
            runner_id: runner.id.clone(),
            dry_run: false,
            mode,
            argv: redact_argv(&command),
            remote_cwd: cwd,
            exit_code: 0,
            stdout,
            stderr: String::new(),
            source_snapshot: Some(source_snapshot.clone()),
            job: Some(job.clone()),
            runner_job: Some(runner_job),
            job_id: Some(job.id.to_string()),
            job_events: None,
            mirror_run_id,
            patch: None,
            mutation_artifacts: None,
            artifacts: Vec::new(),
            metrics: None,
            capture: None,
            runner_result: Some(runner_result),
            handoff: Some(handoff),
            diagnostics: runner_exec_diagnostics(runner, Some(&source_snapshot), &require_paths),
        },
        0,
    )
}

/// Grace window during which a transient daemon polling failure (connection
/// refused while the daemon restarts, a stale tunnel returning `null`, etc.) is
/// retried instead of aborting the wait. A daemon-managed exec job persists its
/// status across restarts, so a brief reconnection gap should not cost the
/// caller the real terminal result of in-flight work (#4770, #3631, #3624).
pub(super) const DAEMON_POLL_TRANSIENT_GRACE: Duration = Duration::from_secs(30);
pub(super) const DAEMON_POLL_RETRY_BACKOFF: Duration = Duration::from_millis(500);

/// Poll a daemon job, tolerating transient failures within the grace window.
///
/// The job store is durable across daemon restarts, so a connection error or a
/// `null` envelope during the restart window is recoverable: the daemon comes
/// back and serves the persisted (and possibly already-terminal) job. Only after
/// the grace window elapses without a successful read do we surface the error,
/// and we annotate it so the caller knows the remote job may still be in flight
/// rather than reporting a misleading hard failure.
pub(super) fn fetch_daemon_job_resilient(
    client: &Client,
    local_url: &str,
    job_id: &str,
) -> Result<Job> {
    let transient_deadline = Instant::now() + DAEMON_POLL_TRANSIENT_GRACE;
    loop {
        match fetch_daemon_job(client, local_url, job_id) {
            Ok(job) => return Ok(job),
            Err(err) => {
                if Instant::now() >= transient_deadline {
                    let mut surfaced = err;
                    surfaced.retryable = surfaced.retryable.or(Some(true));
                    return Err(surfaced.with_hint(format!(
                        "Lost contact with the runner daemon while polling job `{job_id}` for longer than {}s; the remote job may still be in flight. Reconnect with `homeboy runner connect <runner-id>` and inspect `homeboy runner job logs <runner-id> {job_id}`.",
                        DAEMON_POLL_TRANSIENT_GRACE.as_secs()
                    )));
                }
                std::thread::sleep(DAEMON_POLL_RETRY_BACKOFF);
            }
        }
    }
}

pub(super) fn fetch_daemon_events(
    client: &Client,
    local_url: &str,
    job_id: &str,
) -> Result<Vec<JobEvent>> {
    let data = daemon_get(client, local_url, &format!("/jobs/{job_id}/events"))?;
    let body = canonical_daemon_body(&data, "daemon job events response")?;
    serde_json::from_value(body["events"].clone()).map_err(|err| {
        Error::internal_json(err.to_string(), Some("parse daemon job events".to_string()))
    })
}

pub(super) fn daemon_job_context_error(runner_id: &str, job_id: &str, err: Error) -> Error {
    let mut with_context = Error::new(
        err.code,
        err.message,
        json!({
            "runner_id": runner_id,
            "job_id": job_id,
            "source": err.details,
        }),
    );
    with_context.hints = err.hints;
    with_context.retryable = err.retryable.or(Some(true));
    with_context
}

pub(super) fn daemon_job_wait_timeout(
    runner: &Runner,
    cwd: &str,
    command: &[String],
    job: &Job,
    events: &[JobEvent],
    label: &str,
    supports_cancellation: bool,
) -> Error {
    let job_id = job.id.to_string();
    let mirrored = mirror_daemon_job_progress(runner, cwd, command, job, events);
    let mirrored_run_id = mirrored.as_ref().ok().map(|run| run.id.clone());
    let timeout_hint = format!(
        "Set controller-side `{RUNNER_EXEC_WAIT_TIMEOUT_ENV}` before invoking homeboy to change this wait budget, e.g. `{RUNNER_EXEC_WAIT_TIMEOUT_ENV}=2400 homeboy ...`; workload settings are applied inside the remote job and cannot extend the controller wait."
    );
    let mut error = Error::internal_unexpected(format!(
        "{label} {job_id} on runner {} did not finish before timeout; the remote job is still in flight and was not cancelled",
        runner.id
    ));
    error.details["runner_id"] = Value::String(runner.id.clone());
    error.details["job_id"] = Value::String(job_id.clone());
    error.details["remote_cwd"] = Value::String(cwd.to_string());
    error.details["command"] = json!(redact_argv(command));
    match mirrored {
        Ok(run) => {
            error.details["active_run_id"] = Value::String(run.id.clone());
            error = error
                .with_hint(format!(
                    "Mirrored controller timeout state as run `{}`; inspect it with `homeboy runs show {}`.",
                    run.id, run.id
                ))
                .with_hint(format!(
                    "After the remote job finishes, run `homeboy runs artifacts {}` to refresh and list mirrored Lab artifacts without SSH temp-directory spelunking.",
                    run.id
                ));
        }
        Err(err) => {
            error = error.with_hint(format!(
                "Could not persist a local timeout mirror for remote job `{job_id}`: {}",
                err.message
            ));
        }
    }
    for hint in lab_offload_handoff_hints(
        &runner.id,
        Some(cwd),
        &job_id,
        mirrored_run_id.as_deref(),
        DaemonJobHandoffState::InFlight,
        supports_cancellation,
    ) {
        error = error.with_hint(hint);
    }
    error.with_hint(timeout_hint)
}

pub(crate) fn result_event_data(events: &[JobEvent]) -> Option<Value> {
    events
        .iter()
        .rev()
        .find(|event| matches!(event.kind, crate::core::api_jobs::JobEventKind::Result))
        .and_then(|event| event.data.clone())
}

/// Stream + metric fields derived from a runner job's terminal result event.
pub(super) struct RunnerJobResultFields {
    pub(super) result: Value,
    pub(super) stdout: String,
    pub(super) stderr: String,
    pub(super) metrics: Option<RunnerResourceMetrics>,
    pub(super) capture: Option<CommandCaptureMetadata>,
    pub(super) exit_code: i32,
}

/// Extract the terminal result payload from a runner job's events and derive
/// the redacted streams, metrics, and exit code. Shared by the reverse-broker
/// and daemon execution paths to keep their result handling identical (#5067).
pub(super) fn runner_job_result_fields(
    events: &[JobEvent],
    job_status: JobStatus,
    redaction_env: &HashMap<String, String>,
    redaction_secret_env_names: &[String],
) -> RunnerJobResultFields {
    let result = result_event_data(events).unwrap_or_else(|| json!({}));
    let (stdout, stderr) = redact_runner_exec_streams(
        string_field(&result, "stdout"),
        string_field(&result, "stderr"),
        redaction_env,
        redaction_secret_env_names,
    );
    let metrics = result
        .get("metrics")
        .and_then(|value| serde_json::from_value(value.clone()).ok());
    let capture = result
        .get("capture")
        .and_then(|value| serde_json::from_value(value.clone()).ok());
    let exit_code = result
        .get("exit_code")
        .and_then(Value::as_i64)
        .and_then(|code| i32::try_from(code).ok())
        .unwrap_or_else(|| {
            if job_status == JobStatus::Succeeded {
                0
            } else {
                1
            }
        });
    RunnerJobResultFields {
        result,
        stdout,
        stderr,
        metrics,
        capture,
        exit_code,
    }
}
