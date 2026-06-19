use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::core::agent_tasks::{
    provider_secret_sources_for_discovered_providers, secrets as agent_task_secrets,
};
use crate::core::api_jobs::{Job, JobEvent, JobStatus, RemoteRunnerJobRequest};
use crate::core::engine::command::CommandCaptureMetadata;
use crate::core::engine::shell;
use crate::core::error::{Error, ErrorCode, Result};
use crate::core::redaction::RedactionPolicy;
use crate::core::server::{self, SshClient};
use crate::core::source_snapshot::SourceSnapshot;

use super::broker_http;
use super::capabilities::{
    runner_capability_snapshot_for_preflight, validate_runner_capability_preflight,
};
use super::evidence::{mirror_daemon_evidence, mirror_daemon_job_progress};
use super::resource_metrics::{
    measured_command_output, measured_command_output_until_cancelled, RunnerResourceMetrics,
};
use super::{
    connect, load, status, Runner, RunnerCapabilityPreflight, RunnerKind, RunnerTunnelMode,
};
use super::{normalize_runner_command_env, resolve_runner_secret_env};

const DEFAULT_RUNNER_EXEC_WAIT_TIMEOUT_SECS: u64 = 20 * 60;
pub(crate) const RUNNER_EXEC_WAIT_TIMEOUT_ENV: &str = "HOMEBOY_RUNNER_EXEC_WAIT_TIMEOUT_SECS";
pub(crate) const RUNNER_HOSTED_EXEC_ENV: &str = "HOMEBOY_RUNNER_HOSTED_EXEC";

mod extension_parity;
mod policy;
use extension_parity::{required_extensions_for_command, validate_runner_extension_parity};
use policy::{validate_runner_policy, RunnerPolicyRequest};

#[derive(Debug, Clone)]
pub struct RunnerExecOptions {
    pub cwd: Option<String>,
    pub project_id: Option<String>,
    pub allow_diagnostic_ssh: bool,
    pub command: Vec<String>,
    pub env: HashMap<String, String>,
    pub secret_env_names: Vec<String>,
    pub capture_patch: bool,
    pub raw_exec: bool,
    pub source_snapshot: Option<SourceSnapshot>,
    pub capability_preflight: Option<RunnerCapabilityPreflight>,
    pub required_extensions: Vec<String>,
    pub require_paths: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunnerExecMode {
    Daemon,
    Local,
    ReverseBroker,
    DiagnosticSsh,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunnerExecOutput {
    pub command: &'static str,
    pub runner_id: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub dry_run: bool,
    pub mode: RunnerExecMode,
    pub argv: Vec<String>,
    pub remote_cwd: String,
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_snapshot: Option<SourceSnapshot>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job: Option<Job>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job_events: Option<Vec<JobEvent>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mirror_run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub patch: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metrics: Option<RunnerResourceMetrics>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capture: Option<CommandCaptureMetadata>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diagnostics: Option<RunnerExecDiagnostics>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RunnerExecDiagnostics {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runner_workspace_root: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_snapshot_remote_path: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub required_paths: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub hints: Vec<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct RunnerProcessRequest {
    pub runner_id: String,
    pub runner: Option<Runner>,
    pub cwd: Option<String>,
    pub project_id: Option<String>,
    pub command: Vec<String>,
    pub env: HashMap<String, String>,
    pub secret_env_names: Vec<String>,
    pub capture_patch: bool,
    pub raw_exec: bool,
    pub source_snapshot: Option<SourceSnapshot>,
    pub require_paths: Vec<String>,
    pub validate_require_paths_on_host: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct PreparedRunnerProcess {
    pub runner: Runner,
    pub cwd: String,
    pub command: Vec<String>,
    pub env: HashMap<String, String>,
    pub source_snapshot: SourceSnapshot,
    pub require_paths: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct DaemonEnvelope {
    success: bool,
    data: Option<Value>,
    error: Option<Value>,
}

pub fn exec(runner_id: &str, options: RunnerExecOptions) -> Result<(RunnerExecOutput, i32)> {
    if options.command.is_empty() {
        return Err(Error::validation_invalid_argument(
            "command",
            "runner exec requires a command after --",
            None,
            None,
        ));
    }

    let secret_env_names = runner_exec_secret_env_names(
        &options.command,
        options.capability_preflight.as_ref(),
        &options.secret_env_names,
    );
    let plan = prepare_runner_process(RunnerProcessRequest {
        runner_id: runner_id.to_string(),
        runner: None,
        cwd: options.cwd.clone(),
        project_id: options.project_id.clone(),
        command: options.command.clone(),
        env: options.env.clone(),
        secret_env_names: secret_env_names.clone(),
        capture_patch: options.capture_patch,
        raw_exec: options.raw_exec,
        source_snapshot: options.source_snapshot.clone(),
        require_paths: options.require_paths.clone(),
        validate_require_paths_on_host: false,
    })?;
    let runner = plan.runner.clone();
    let cwd = plan.cwd.clone();
    let request_env = plan.env.clone();
    let required_extensions =
        required_extensions_for_command(&options.command, &options.required_extensions);

    validate_runner_extension_parity(runner_id, &runner, &cwd, &required_extensions)?;

    let run_capability_preflight = |runner: &Runner| -> Result<()> {
        preflight_runner_capability_plan(
            runner,
            options.capability_preflight.as_ref(),
            &request_env,
        )
    };

    if should_force_diagnostic_ssh(&runner, &options) {
        run_capability_preflight(&runner)?;
        return exec_diagnostic_ssh(
            &runner,
            cwd,
            options.command,
            request_env,
            options.require_paths,
        );
    }

    let mut connected = status(runner_id)?;
    if connected.connected && connected.stale_daemon.is_some() {
        let (refresh, refresh_exit_code) = connect(runner_id)?;
        if refresh_exit_code != 0 || !refresh.connected {
            return Err(Error::internal_unexpected(
                format!(
                    "runner `{runner_id}` has a stale daemon session and automatic refresh failed: {}",
                    refresh
                        .failure_message
                        .as_deref()
                        .unwrap_or("runner connect did not establish a fresh daemon session")
                )
            )
            .with_hint(format!(
                "Refresh the runner session with `homeboy runner disconnect {runner_id} && homeboy runner connect {runner_id}`."
            )));
        }
        connected = status(runner_id)?;
    }
    if connected.connected {
        if let Some(session) = connected.session {
            run_capability_preflight(&runner)?;
            if let Some(local_url) = session.local_url.as_deref() {
                return exec_via_daemon(
                    &runner,
                    local_url,
                    cwd,
                    options.project_id,
                    options.command,
                    request_env,
                    secret_env_names,
                    options.capture_patch,
                    Some(plan.source_snapshot),
                    options.require_paths,
                );
            }
            if session.mode == RunnerTunnelMode::Reverse {
                if let Some(broker_url) = session.broker_url.as_deref() {
                    return exec_via_reverse_broker(
                        &runner,
                        broker_url,
                        cwd,
                        options.project_id,
                        options.command,
                        request_env,
                        secret_env_names,
                        options.capture_patch,
                        Some(plan.source_snapshot),
                        options.require_paths,
                    );
                }
            }
        }
    }

    match runner.kind {
        RunnerKind::Local => exec_local(plan),
        RunnerKind::Ssh => Err(Error::validation_invalid_argument(
            "runner",
            "runner is not connected to a daemon; run `homeboy runner connect <runner-id>` or pass `--ssh` for explicit SSH diagnostics",
            Some(runner.id),
            Some(vec![
                "Daemon-backed execution preserves job metadata and artifact discovery.".to_string(),
                "SSH execution is intended for MVP diagnostics and must be explicit.".to_string(),
            ]),
        )),
    }
}

pub(crate) fn exec_worker_local(
    runner_id: &str,
    options: RunnerExecOptions,
) -> Result<(RunnerExecOutput, i32)> {
    let secret_env_names = runner_exec_secret_env_names(
        &options.command,
        options.capability_preflight.as_ref(),
        &options.secret_env_names,
    );
    let mut runner = load(runner_id)?;
    runner.kind = RunnerKind::Local;
    runner.server_id = None;
    let plan = prepare_daemon_local_process(RunnerProcessRequest {
        runner_id: runner_id.to_string(),
        runner: Some(runner),
        cwd: options.cwd.clone(),
        project_id: options.project_id.clone(),
        command: options.command.clone(),
        env: options.env.clone(),
        secret_env_names,
        capture_patch: options.capture_patch,
        raw_exec: options.raw_exec,
        source_snapshot: options.source_snapshot.clone(),
        require_paths: options.require_paths.clone(),
        validate_require_paths_on_host: true,
    })?;
    validate_runner_policy(
        &plan.runner,
        &plan.cwd,
        RunnerPolicyRequest {
            project_id: options.project_id.as_deref(),
            command: &options.command,
            capture_patch: options.capture_patch,
            raw_exec: options.raw_exec,
        },
    )?;
    preflight_worker_local_capability_plan(
        &plan.runner,
        options.capability_preflight.as_ref(),
        &plan.env,
    )?;
    exec_local(plan)
}

fn preflight_worker_local_capability_plan(
    runner: &Runner,
    preflight: Option<&RunnerCapabilityPreflight>,
    request_env: &HashMap<String, String>,
) -> Result<()> {
    let Some(preflight) = preflight else {
        return Ok(());
    };
    if preflight.is_empty() {
        return Ok(());
    }

    let capabilities = runner_capability_snapshot_for_preflight(runner, preflight)?;
    validate_runner_capability_preflight(&runner.id, preflight, &capabilities, request_env)
}

fn should_force_diagnostic_ssh(runner: &Runner, options: &RunnerExecOptions) -> bool {
    runner.kind == RunnerKind::Ssh && options.allow_diagnostic_ssh
}

pub fn runner_exec_failure_error(output: &RunnerExecOutput) -> Option<Error> {
    if output.exit_code == 0 {
        return None;
    }

    let runner_error = find_runner_homeboy_error(output);
    let runner_code = runner_error
        .as_ref()
        .and_then(|error| error.get("code"))
        .and_then(Value::as_str);
    let runner_message = runner_error
        .as_ref()
        .and_then(|error| error.get("message"))
        .and_then(Value::as_str);
    let cause = runner_message
        .or(runner_code)
        .or_else(|| first_non_empty_line(&output.stderr))
        .or_else(|| first_non_empty_line(&output.stdout))
        .unwrap_or("runner command exited non-zero")
        .to_string();
    let execution = serde_json::to_value(output).unwrap_or(Value::Null);
    let command = output.argv.join(" ");
    let mut details = json!({
        "runner_id": output.runner_id,
        "job_id": output.job_id,
        "remote_cwd": output.remote_cwd,
        "command": output.argv,
        "exit_code": output.exit_code,
        "execution": execution,
    });
    if let Some(runner_error) = runner_error {
        details["runner_error"] = runner_error;
    }

    Some(
        Error::new(
            ErrorCode::RemoteCommandFailed,
            format!(
                "Runner command failed on `{}` with exit code {}: {}",
                output.runner_id, output.exit_code, cause
            ),
            details,
        )
        .with_hint(format!(
            "Runner `{}` executed `{}` from `{}`.", output.runner_id, command, output.remote_cwd
        ))
        .with_hint(
            "Homeboy parsed runner-side JSON errors from stdout, stderr, and job event messages when present; inspect error.details.execution for the full job evidence."
                .to_string(),
        ),
    )
}

#[allow(clippy::too_many_arguments)]
fn exec_via_reverse_broker(
    runner: &Runner,
    broker_url: &str,
    cwd: String,
    project_id: Option<String>,
    command: Vec<String>,
    env: HashMap<String, String>,
    secret_env_names: Vec<String>,
    capture_patch: bool,
    source_snapshot_override: Option<SourceSnapshot>,
    require_paths: Vec<String>,
) -> Result<(RunnerExecOutput, i32)> {
    let client = Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|err| Error::internal_unexpected(format!("build broker HTTP client: {err}")))?;
    let source_snapshot = source_snapshot_override.unwrap_or_else(|| {
        SourceSnapshot::existing_remote(&runner.id, &cwd, runner.workspace_root.as_deref())
    });
    let redaction_env = env.clone();
    let redaction_secret_env_names = secret_env_names.clone();
    let request = RemoteRunnerJobRequest {
        runner_id: runner.id.clone(),
        project_id,
        operation: "runner.exec".to_string(),
        command: command.clone(),
        cwd: Some(cwd.clone()),
        env,
        secret_env_names,
        capture_patch,
        source_snapshot: Some(source_snapshot.clone()),
        metadata: Some(json!({
            "transport": "reverse_broker",
        })),
        require_paths: require_paths.clone(),
    };
    let data = broker_http::post_json(
        &client,
        broker_url,
        "/runner/jobs",
        serde_json::to_value(&request).map_err(|err| {
            Error::internal_json(
                err.to_string(),
                Some("serialize reverse runner job request".to_string()),
            )
        })?,
        "submit reverse runner job",
    )?;
    let job_value = data
        .get("job")
        .ok_or_else(|| Error::internal_unexpected("reverse broker submit returned no job"))?;
    let mut job: Job = serde_json::from_value(job_value.clone()).map_err(|err| {
        Error::internal_json(
            err.to_string(),
            Some("parse reverse broker job".to_string()),
        )
    })?;
    persist_lab_offload_handoff_run(runner, &cwd, &command, &job);

    let deadline = Instant::now() + runner_exec_wait_timeout();
    while !matches!(
        job.status,
        JobStatus::Succeeded | JobStatus::Failed | JobStatus::Cancelled
    ) {
        if Instant::now() >= deadline {
            let events = fetch_daemon_events(&client, broker_url, &job.id.to_string())
                .map(|events| {
                    redact_runner_job_events(&events, &redaction_env, &redaction_secret_env_names)
                })
                .unwrap_or_default();
            return Err(daemon_job_wait_timeout(
                runner,
                &cwd,
                &command,
                &job,
                &events,
                "reverse runner job",
            ));
        }
        std::thread::sleep(Duration::from_millis(200));
        let job_id = job.id.to_string();
        job = fetch_daemon_job_resilient(&client, broker_url, &job_id)
            .map_err(|err| daemon_job_context_error(&runner.id, &job_id, err))?;
    }
    let events = redact_runner_job_events(
        &fetch_daemon_events(&client, broker_url, &job.id.to_string())?,
        &redaction_env,
        &redaction_secret_env_names,
    );

    let RunnerJobResultFields {
        result: _,
        stdout,
        stderr,
        metrics,
        exit_code,
    } = runner_job_result_fields(
        &events,
        job.status,
        &redaction_env,
        &redaction_secret_env_names,
    );

    print_lab_offload_handoff(
        &runner.id,
        Some(&cwd),
        &job.id.to_string(),
        None,
        DaemonJobHandoffState::Terminal(job.status),
    );

    Ok((
        RunnerExecOutput {
            command: "runner.exec",
            runner_id: runner.id.clone(),
            dry_run: false,
            mode: RunnerExecMode::ReverseBroker,
            argv: command,
            remote_cwd: cwd,
            exit_code,
            stdout,
            stderr,
            source_snapshot: Some(source_snapshot.clone()),
            job_id: Some(job.id.to_string()),
            job: Some(job),
            job_events: Some(events),
            mirror_run_id: None,
            patch: None,
            metrics,
            capture: None,
            diagnostics: runner_exec_diagnostics(runner, Some(&source_snapshot), &require_paths),
        },
        exit_code,
    ))
}

#[allow(clippy::too_many_arguments)]
fn exec_via_daemon(
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
    persist_lab_offload_handoff_run(runner, &cwd, &command, &job);

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
        exit_code,
    } = runner_job_result_fields(&events, job.status, &env, &secret_env_names);

    let mirror = mirror_daemon_evidence(runner, &cwd, &command, &job, &events, &result)?;
    let patch = mirror.as_ref().and_then(|evidence| evidence.patch.clone());
    let mirror_run_id = mirror.as_ref().map(|evidence| evidence.run.id.as_str());
    print_lab_offload_handoff(
        &runner.id,
        Some(&cwd),
        &job.id.to_string(),
        mirror_run_id,
        DaemonJobHandoffState::Terminal(job.status),
    );

    Ok((
        RunnerExecOutput {
            command: "runner.exec",
            runner_id: runner.id.clone(),
            dry_run: false,
            mode: RunnerExecMode::Daemon,
            argv: command,
            remote_cwd: cwd,
            exit_code,
            stdout,
            stderr,
            source_snapshot: Some(source_snapshot.clone()),
            job_id: Some(job.id.to_string()),
            job: Some(job),
            job_events: Some(events),
            mirror_run_id: mirror.map(|evidence| evidence.run.id),
            patch,
            metrics,
            capture: None,
            diagnostics: runner_exec_diagnostics(runner, Some(&source_snapshot), &require_paths),
        },
        exit_code,
    ))
}

fn preflight_runner_capability_plan(
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

fn fetch_daemon_job(client: &Client, local_url: &str, job_id: &str) -> Result<Job> {
    let data = daemon_get(client, local_url, &format!("/jobs/{job_id}"))?;
    let body = canonical_daemon_body(&data, "daemon job response")?;
    serde_json::from_value(body["job"].clone())
        .map_err(|err| Error::internal_json(err.to_string(), Some("parse daemon job".to_string())))
}

/// Grace window during which a transient daemon polling failure (connection
/// refused while the daemon restarts, a stale tunnel returning `null`, etc.) is
/// retried instead of aborting the wait. A daemon-managed exec job persists its
/// status across restarts, so a brief reconnection gap should not cost the
/// caller the real terminal result of in-flight work (#4770, #3631, #3624).
const DAEMON_POLL_TRANSIENT_GRACE: Duration = Duration::from_secs(30);
const DAEMON_POLL_RETRY_BACKOFF: Duration = Duration::from_millis(500);

/// Poll a daemon job, tolerating transient failures within the grace window.
///
/// The job store is durable across daemon restarts, so a connection error or a
/// `null` envelope during the restart window is recoverable: the daemon comes
/// back and serves the persisted (and possibly already-terminal) job. Only after
/// the grace window elapses without a successful read do we surface the error,
/// and we annotate it so the caller knows the remote job may still be in flight
/// rather than reporting a misleading hard failure.
fn fetch_daemon_job_resilient(client: &Client, local_url: &str, job_id: &str) -> Result<Job> {
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

fn fetch_daemon_events(client: &Client, local_url: &str, job_id: &str) -> Result<Vec<JobEvent>> {
    let data = daemon_get(client, local_url, &format!("/jobs/{job_id}/events"))?;
    let body = canonical_daemon_body(&data, "daemon job events response")?;
    serde_json::from_value(body["events"].clone()).map_err(|err| {
        Error::internal_json(err.to_string(), Some("parse daemon job events".to_string()))
    })
}

fn daemon_job_context_error(runner_id: &str, job_id: &str, err: Error) -> Error {
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

fn daemon_job_wait_timeout(
    runner: &Runner,
    cwd: &str,
    command: &[String],
    job: &Job,
    events: &[JobEvent],
    label: &str,
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
    error.details["command"] = json!(command);
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
    ) {
        error = error.with_hint(hint);
    }
    error.with_hint(timeout_hint)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DaemonJobHandoffState {
    InFlight,
    Terminal(JobStatus),
}

pub(crate) fn lab_offload_handoff_hints(
    runner_id: &str,
    remote_cwd: Option<&str>,
    job_id: &str,
    persisted_run_id: Option<&str>,
    state: DaemonJobHandoffState,
) -> Vec<String> {
    let runner_exec_prefix = match remote_cwd.filter(|cwd| !cwd.trim().is_empty()) {
        Some(cwd) => format!(
            "homeboy runner exec {runner_id} --cwd {} --",
            shell::quote_arg(cwd)
        ),
        None => format!("homeboy runner exec {runner_id} --"),
    };
    let remote_run_filter =
        format!("{runner_exec_prefix} homeboy runs list --status running --limit 20");
    let mut hints = match state {
        DaemonJobHandoffState::InFlight => vec![format!(
            "Lab offload handoff: runner `{runner_id}` has daemon job `{job_id}` still in flight; the runner-side Homeboy command may continue after this controller exits."
        )],
        DaemonJobHandoffState::Terminal(status) => vec![format!(
            "Lab offload handoff: runner `{runner_id}` daemon job `{job_id}` finished with status `{}`.",
            status.daemon_status_label()
        )],
    };

    if let Some(run_id) = persisted_run_id.filter(|run_id| !run_id.trim().is_empty()) {
        hints.push(format!(
            "Persisted run id: `{run_id}`. Status: `homeboy runs show {run_id}`; evidence: `homeboy runs evidence {run_id}`; artifacts: `homeboy runs artifacts {run_id}`."
        ));
    } else if state == DaemonJobHandoffState::InFlight {
        hints.push(format!(
            "Persisted runner-side run id is not known yet; list active runner runs with `{remote_run_filter}`."
        ));
    } else {
        hints.push(
            "Persisted runner-side run id is not known; inspect daemon job events for final result details."
                .to_string(),
        );
    }

    match state {
        DaemonJobHandoffState::InFlight => hints.push(format!(
            "Runner-side status/evidence/artifacts: `{remote_run_filter}` then `{runner_exec_prefix} homeboy runs show <run-id>`, `{runner_exec_prefix} homeboy runs evidence <run-id>`, and `{runner_exec_prefix} homeboy runs artifacts <run-id>`."
        )),
        DaemonJobHandoffState::Terminal(_) => hints.push(format!(
            "Final daemon job events/result: `homeboy runner job logs {runner_id} {job_id}`."
        )),
    }
    hints.push(format!(
        "Daemon job logs: `homeboy runner job logs {runner_id} {job_id} --follow`."
    ));
    if state == DaemonJobHandoffState::InFlight {
        hints.push(format!(
            "Cancel if supported: `homeboy runner job cancel {runner_id} {job_id}`."
        ));
    }
    hints
}

fn print_lab_offload_handoff(
    runner_id: &str,
    remote_cwd: Option<&str>,
    job_id: &str,
    persisted_run_id: Option<&str>,
    state: DaemonJobHandoffState,
) {
    eprintln!("Lab offload handoff:");
    for hint in lab_offload_handoff_hints(runner_id, remote_cwd, job_id, persisted_run_id, state) {
        eprintln!("- {hint}");
    }
}

fn persist_lab_offload_handoff_run(
    runner: &Runner,
    cwd: &str,
    command: &[String],
    job: &Job,
) -> Option<String> {
    match mirror_daemon_job_progress(runner, cwd, command, job, &[]) {
        Ok(run) => Some(run.id),
        Err(err) => {
            eprintln!(
                "Lab offload handoff: could not persist controller-side run mirror for runner `{}` daemon job `{}`: {}",
                runner.id, job.id, err.message
            );
            None
        }
    }
}

fn runner_exec_wait_timeout() -> Duration {
    std::env::var(RUNNER_EXEC_WAIT_TIMEOUT_ENV)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or_else(|| Duration::from_secs(DEFAULT_RUNNER_EXEC_WAIT_TIMEOUT_SECS))
}

pub(crate) fn canonical_daemon_body<'a>(data: &'a Value, context: &str) -> Result<&'a Value> {
    data.get("body")
        .ok_or_else(|| Error::internal_unexpected(format!("{context} missing canonical data.body")))
}

fn daemon_get(client: &Client, local_url: &str, path: &str) -> Result<Value> {
    let response = client
        .get(format!("{}{}", local_url.trim_end_matches('/'), path))
        .send()
        .map_err(|err| Error::internal_unexpected(format!("query runner daemon: {err}")))?;
    let envelope: DaemonEnvelope = response.json().map_err(|err| {
        Error::internal_json(err.to_string(), Some("parse daemon response".to_string()))
    })?;
    if !envelope.success {
        return Err(Error::internal_unexpected(format!(
            "daemon request failed: {}",
            envelope.error.unwrap_or(Value::Null)
        )));
    }
    envelope
        .data
        .ok_or_else(|| Error::internal_unexpected("daemon response missing data"))
}

pub(crate) fn daemon_api_get(runner_id: &str, path: &str) -> Result<Value> {
    daemon_api_request(runner_id, path, "GET")
}

pub fn daemon_api_post(runner_id: &str, path: &str) -> Result<Value> {
    daemon_api_request(runner_id, path, "POST")
}

fn daemon_api_request(runner_id: &str, path: &str, method: &str) -> Result<Value> {
    let runner = load(runner_id)?;
    let connected = status(runner_id)?;
    let Some(session) = connected.session.filter(|_| connected.connected) else {
        return Err(Error::validation_invalid_argument(
            "runner",
            "runner is not connected to a daemon; run `homeboy runner connect <runner-id>` first",
            Some(runner.id),
            Some(vec![
                "Read/query integrations use the connected daemon so results come from the runner machine.".to_string(),
            ]),
        ));
    };
    let client = Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|err| Error::internal_unexpected(format!("build daemon HTTP client: {err}")))?;
    let Some(local_url) = session.local_url.as_deref() else {
        return Err(Error::validation_invalid_argument(
            "runner",
            "runner session does not expose a local daemon URL yet",
            Some(runner.id),
            Some(vec![
                "Reverse tunnel daemon routing is tracked in #2946 and #2948.".to_string(),
            ]),
        ));
    };
    match method {
        "GET" => daemon_get(&client, local_url, path),
        "POST" => daemon_post(&client, local_url, path),
        _ => Err(Error::internal_unexpected(format!(
            "unsupported daemon API method {method}"
        ))),
    }
}

fn daemon_post(client: &Client, local_url: &str, path: &str) -> Result<Value> {
    let response = client
        .post(format!("{}{}", local_url.trim_end_matches('/'), path))
        .send()
        .map_err(|err| Error::internal_unexpected(format!("query runner daemon: {err}")))?;
    let envelope: DaemonEnvelope = response.json().map_err(|err| {
        Error::internal_json(err.to_string(), Some("parse daemon response".to_string()))
    })?;
    if !envelope.success {
        return Err(Error::internal_unexpected(format!(
            "daemon request failed: {}",
            envelope.error.unwrap_or(Value::Null)
        )));
    }
    envelope
        .data
        .ok_or_else(|| Error::internal_unexpected("daemon response missing data"))
}

pub(super) fn result_event_data(events: &[JobEvent]) -> Option<Value> {
    events
        .iter()
        .rev()
        .find(|event| matches!(event.kind, crate::core::api_jobs::JobEventKind::Result))
        .and_then(|event| event.data.clone())
}

/// Stream + metric fields derived from a runner job's terminal result event.
struct RunnerJobResultFields {
    result: Value,
    stdout: String,
    stderr: String,
    metrics: Option<RunnerResourceMetrics>,
    exit_code: i32,
}

/// Extract the terminal result payload from a runner job's events and derive
/// the redacted streams, metrics, and exit code. Shared by the reverse-broker
/// and daemon execution paths to keep their result handling identical (#5067).
fn runner_job_result_fields(
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
        exit_code,
    }
}

fn exec_local(plan: PreparedRunnerProcess) -> Result<(RunnerExecOutput, i32)> {
    let output = execute_runner_process(&plan)?;
    Ok(exec_output(
        &plan.runner,
        RunnerExecMode::Local,
        plan.cwd,
        plan.command,
        output,
        Some(plan.source_snapshot),
        plan.require_paths,
        &plan.env,
        &[],
    ))
}

fn exec_diagnostic_ssh(
    runner: &Runner,
    cwd: String,
    command: Vec<String>,
    env: HashMap<String, String>,
    require_paths: Vec<String>,
) -> Result<(RunnerExecOutput, i32)> {
    let server_id = runner.server_id.as_deref().ok_or_else(|| {
        Error::validation_invalid_argument(
            "server_id",
            "SSH runner requires server_id",
            Some(runner.id.clone()),
            None,
        )
    })?;
    let server = server::load(server_id)?;
    let mut client = SshClient::from_server(&server, server_id)?;
    client.env.extend(runner.env.clone());
    client.env.extend(env);
    let redaction_env = client.env.clone();
    validate_remote_required_paths(&mut client, &require_paths)?;
    let command_line = format!(
        "cd {} && {}",
        shell::quote_arg(&cwd),
        command
            .iter()
            .map(|arg| shell::quote_arg(arg))
            .collect::<Vec<_>>()
            .join(" ")
    );
    let output = client.execute(&command_line);
    let source_snapshot =
        SourceSnapshot::existing_remote(&runner.id, &cwd, runner.workspace_root.as_deref());
    Ok(exec_output(
        runner,
        RunnerExecMode::DiagnosticSsh,
        cwd,
        command,
        ProcessOutput {
            stdout: output.stdout,
            stderr: output.stderr,
            exit_code: output.exit_code,
            metrics: None,
            capture: None,
        },
        Some(source_snapshot),
        require_paths,
        &redaction_env,
        &[],
    ))
}

pub(crate) struct ProcessOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
    pub metrics: Option<RunnerResourceMetrics>,
    pub capture: Option<CommandCaptureMetadata>,
}

pub(crate) fn prepare_runner_process(
    request: RunnerProcessRequest,
) -> Result<PreparedRunnerProcess> {
    if request.command.is_empty() {
        return Err(Error::validation_invalid_argument(
            "command",
            "runner exec requires a command after --",
            None,
            None,
        ));
    }

    let runner = request
        .runner
        .map(|mut runner| {
            if runner.id.is_empty() {
                runner.id = request.runner_id.clone();
            }
            runner
        })
        .map(Ok)
        .unwrap_or_else(|| load(&request.runner_id))?;
    let cwd = resolve_cwd(&runner, request.cwd.as_deref())?;
    validate_runner_process_cwd(&runner, &cwd)?;
    if runner.kind != RunnerKind::Local {
        super::source_materialization::validate_runner_exec_source_fetch(
            &request.command,
            &runner.id,
        )?;
        provision_provider_file_secret_sources_for_runner(
            &runner,
            &request.command,
            &request.secret_env_names,
            &request.env,
        )?;
    }
    validate_runner_policy(
        &runner,
        &cwd,
        RunnerPolicyRequest {
            project_id: request.project_id.as_deref(),
            command: &request.command,
            capture_patch: request.capture_patch,
            raw_exec: request.raw_exec,
        },
    )?;

    let mut env = runner.env.clone();
    env.extend(request.env);
    if runner.kind != RunnerKind::Local {
        env.insert(RUNNER_HOSTED_EXEC_ENV.to_string(), "1".to_string());
    }
    if runner.kind == RunnerKind::Local {
        env.extend(resolve_runner_secret_env_for_command(
            &runner.secret_env,
            &request.secret_env_names,
            &env,
        )?);
        normalize_runner_command_env(&mut env);
    } else {
        env.extend(resolve_controller_secret_env_for_command(
            &runner.secret_env,
            &request.secret_env_names,
            &env,
        )?);
    }

    let source_snapshot = request
        .source_snapshot
        .unwrap_or_else(|| match runner.kind {
            RunnerKind::Local => SourceSnapshot::collect_local(
                &runner.id,
                Path::new(&cwd),
                Some(&cwd),
                "existing_remote",
            ),
            RunnerKind::Ssh => {
                SourceSnapshot::existing_remote(&runner.id, &cwd, runner.workspace_root.as_deref())
            }
        });
    validate_required_paths(
        &runner,
        &request.require_paths,
        request.validate_require_paths_on_host,
    )?;

    Ok(PreparedRunnerProcess {
        runner,
        cwd,
        command: request.command,
        env,
        source_snapshot,
        require_paths: request.require_paths,
    })
}

pub(crate) fn prepare_daemon_local_process(
    request: RunnerProcessRequest,
) -> Result<PreparedRunnerProcess> {
    if request.command.is_empty() {
        return Err(Error::validation_invalid_argument(
            "command",
            "runner exec requires a command after --",
            None,
            None,
        ));
    }

    let cwd = request.cwd.ok_or_else(|| {
        Error::validation_invalid_argument(
            "cwd",
            "daemon exec requires an absolute cwd",
            Some(request.runner_id.clone()),
            Some(vec![
                "Pass the synced remote workspace path as cwd when submitting daemon exec."
                    .to_string(),
            ]),
        )
    })?;
    let runner = request
        .runner
        .map(|mut runner| {
            if runner.id.is_empty() {
                runner.id = request.runner_id.clone();
            }
            runner.kind = RunnerKind::Local;
            runner.server_id = None;
            runner.workspace_root = runner.workspace_root.or_else(|| Some(cwd.clone()));
            runner
        })
        .unwrap_or_else(|| Runner {
            id: request.runner_id,
            kind: RunnerKind::Local,
            server_id: None,
            workspace_root: Some(cwd.clone()),
            settings: server::RunnerSettings::default(),
            env: HashMap::new(),
            secret_env: HashMap::new(),
            resources: HashMap::new(),
            policy: server::RunnerPolicy::default(),
        });
    validate_runner_process_cwd(&runner, &cwd)?;
    validate_required_paths(
        &runner,
        &request.require_paths,
        request.validate_require_paths_on_host,
    )?;

    let mut env = runner.env.clone();
    env.extend(request.env);
    env.extend(resolve_runner_secret_env_for_command(
        &runner.secret_env,
        &request.secret_env_names,
        &env,
    )?);
    normalize_runner_command_env(&mut env);
    let source_snapshot = request.source_snapshot.unwrap_or_else(|| {
        SourceSnapshot::collect_local(&runner.id, Path::new(&cwd), Some(&cwd), "existing_remote")
    });

    Ok(PreparedRunnerProcess {
        runner,
        cwd,
        command: request.command,
        env,
        source_snapshot,
        require_paths: request.require_paths,
    })
}

fn resolve_runner_secret_env_for_command(
    secret_env: &HashMap<String, server::RunnerSecretEnvRef>,
    required_names: &[String],
    env: &HashMap<String, String>,
) -> Result<HashMap<String, String>> {
    resolve_runner_secret_env_for_command_with_fallbacks(
        secret_env,
        required_names,
        env,
        &provider_secret_sources_for_discovered_providers(),
    )
}

fn resolve_runner_secret_env_for_command_with_fallbacks(
    secret_env: &HashMap<String, server::RunnerSecretEnvRef>,
    required_names: &[String],
    env: &HashMap<String, String>,
    fallback_sources: &HashMap<String, crate::core::defaults::AgentTaskSecretSource>,
) -> Result<HashMap<String, String>> {
    if required_names.is_empty() {
        return Ok(HashMap::new());
    }

    let mut refs = HashMap::new();
    let mut resolved = HashMap::new();
    for name in required_names {
        if env.contains_key(name.as_str()) {
            continue;
        }
        if let Some(source) = secret_env.get(name.as_str()) {
            refs.insert(name.clone(), source.clone());
            continue;
        }
        if fallback_sources.contains_key(name) {
            if let Ok(values) = agent_task_secrets::resolve_secret_env_with_fallbacks(
                std::slice::from_ref(name),
                &fallback_sources,
            ) {
                for (name, value) in values {
                    resolved.insert(name, value);
                }
                continue;
            }
        }
        return Err(Error::validation_invalid_argument(
            "secret_env",
            format!("missing runner secret env ref for {name}"),
            Some(name.clone()),
            Some(vec![
                "Configure the selected runner secret_env reference, declare provider secret_env_sources that resolve on the runner, or pass the secret in the exec request environment.".to_string(),
            ]),
        ));
    }

    resolved.extend(resolve_runner_secret_env(&refs)?);
    Ok(resolved)
}

fn provision_provider_file_secret_sources_for_runner(
    runner: &Runner,
    command: &[String],
    required_names: &[String],
    request_env: &HashMap<String, String>,
) -> Result<()> {
    if !is_agent_task_run_plan_command(command) || required_names.is_empty() {
        return Ok(());
    }
    let fallback_sources = provider_secret_sources_for_discovered_providers();
    let provisions = provider_file_secret_source_provisions(required_names, &fallback_sources);
    if provisions.is_empty() {
        return Ok(());
    }

    let server_id = runner.server_id.as_deref().ok_or_else(|| {
        Error::validation_invalid_argument(
            "runner",
            "SSH runner requires server_id before provider secret source provisioning",
            Some(runner.id.clone()),
            None,
        )
    })?;
    let server = server::load(server_id)?;
    let client = SshClient::from_server(&server, server_id)?;
    for provision in provisions {
        if provision
            .env_names
            .iter()
            .all(|name| request_env.contains_key(name.as_str()))
        {
            continue;
        }
        agent_task_secrets::resolve_secret_env_with_fallbacks(
            &provision.env_names,
            &fallback_sources,
        )
        .map_err(|err| {
            provider_file_secret_source_error(
                &runner.id,
                &provision,
                format!(
                    "controller credential source does not satisfy provider env names: {}",
                    err.message
                ),
            )
        })?;
        provision_provider_file_secret_source(&client, &runner.id, &provision)?;
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProviderFileSecretSourceProvision {
    path: String,
    env_names: Vec<String>,
}

fn provider_file_secret_source_provisions(
    required_names: &[String],
    fallback_sources: &HashMap<String, crate::core::defaults::AgentTaskSecretSource>,
) -> Vec<ProviderFileSecretSourceProvision> {
    let mut by_path: HashMap<String, Vec<String>> = HashMap::new();
    for name in required_names {
        let Some(source) = fallback_sources.get(name) else {
            continue;
        };
        if source.source != "json-file" {
            continue;
        }
        let Some(path) = source
            .path
            .as_deref()
            .filter(|path| !path.trim().is_empty())
        else {
            continue;
        };
        by_path
            .entry(path.to_string())
            .or_default()
            .push(name.clone());
    }

    let mut provisions = by_path
        .into_iter()
        .map(|(path, mut env_names)| {
            env_names.sort();
            env_names.dedup();
            ProviderFileSecretSourceProvision { path, env_names }
        })
        .collect::<Vec<_>>();
    provisions.sort_by(|left, right| left.path.cmp(&right.path));
    provisions
}

fn provision_provider_file_secret_source(
    client: &SshClient,
    runner_id: &str,
    provision: &ProviderFileSecretSourceProvision,
) -> Result<()> {
    let local_path = expanded_home_path(&provision.path);
    let local_raw = std::fs::read_to_string(&local_path).map_err(|err| {
        provider_file_secret_source_error(
            runner_id,
            provision,
            format!("controller credential source is not readable: {err}"),
        )
    })?;
    let remote_path = remote_secret_source_path(client, &provision.path)?;
    let Some(parent) = Path::new(&remote_path).parent().and_then(Path::to_str) else {
        return Err(provider_file_secret_source_error(
            runner_id,
            provision,
            "runner credential source path has no parent directory".to_string(),
        ));
    };

    let prepare = client.execute(&format!(
        "mkdir -p {} && chmod 700 {}",
        shell::quote_arg(parent),
        shell::quote_arg(parent)
    ));
    if !prepare.success {
        return Err(provider_file_secret_source_error(
            runner_id,
            provision,
            "failed to prepare runner credential directory".to_string(),
        ));
    }

    let temp = tempfile::NamedTempFile::new().map_err(|err| {
        Error::internal_io(
            err.to_string(),
            Some("create provider credential temp file".to_string()),
        )
    })?;
    std::fs::write(temp.path(), local_raw).map_err(|err| {
        Error::internal_io(
            err.to_string(),
            Some("write provider credential temp file".to_string()),
        )
    })?;
    let upload = client.upload_file(&temp.path().to_string_lossy(), &remote_path);
    if !upload.success {
        return Err(provider_file_secret_source_error(
            runner_id,
            provision,
            "failed to upload credential source to runner".to_string(),
        ));
    }
    let chmod = client.execute(&format!("chmod 600 {}", shell::quote_arg(&remote_path)));
    if !chmod.success {
        return Err(provider_file_secret_source_error(
            runner_id,
            provision,
            "failed to lock down runner credential source permissions".to_string(),
        ));
    }
    Ok(())
}

fn provider_file_secret_source_error(
    runner_id: &str,
    provision: &ProviderFileSecretSourceProvision,
    reason: String,
) -> Error {
    Error::validation_invalid_argument(
        "secret_env",
        format!(
            "provider runner credential source for {} cannot be provisioned on runner `{}`: {}",
            provision.env_names.join(", "),
            runner_id,
            reason
        ),
        Some(runner_id.to_string()),
        Some(vec![
            "Refresh the provider credentials on the controller, then rerun the Lab offload so Homeboy can provision the runner-side credential source before dispatch.".to_string(),
            "Credential values are not printed; inspect provider auth with the provider's own auth status command if refresh continues to fail.".to_string(),
        ]),
    )
}

fn remote_secret_source_path(client: &SshClient, path: &str) -> Result<String> {
    if path == "~" || path.starts_with("~/") {
        let home = client.execute("printf %s \"$HOME\"");
        if !home.success || home.stdout.trim().is_empty() {
            return Err(Error::internal_unexpected(
                "failed to resolve runner home directory for provider credential source",
            ));
        }
        let suffix = path.strip_prefix('~').unwrap_or_default();
        return Ok(format!("{}{}", home.stdout.trim_end_matches('/'), suffix));
    }
    Ok(path.to_string())
}

fn expanded_home_path(path: &str) -> PathBuf {
    PathBuf::from(shellexpand::tilde(path).to_string())
}

fn is_agent_task_run_plan_command(command: &[String]) -> bool {
    command
        .windows(2)
        .any(|items| items[0] == "agent-task" && items[1] == "run-plan")
}

fn resolve_controller_secret_env_for_command(
    secret_env: &HashMap<String, server::RunnerSecretEnvRef>,
    required_names: &[String],
    env: &HashMap<String, String>,
) -> Result<HashMap<String, String>> {
    let mut resolved = HashMap::new();
    let fallback_sources = provider_secret_sources_for_discovered_providers();
    for name in required_names {
        if env.contains_key(name.as_str()) {
            continue;
        }
        let Some(source) = secret_env.get(name.as_str()) else {
            continue;
        };
        if source
            .secret
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
        {
            resolved.extend(resolve_runner_secret_env(&HashMap::from([(
                name.clone(),
                source.clone(),
            )]))?);
            continue;
        }

        if fallback_sources.contains_key(name) {
            if let Ok(values) = agent_task_secrets::resolve_secret_env_with_fallbacks(
                std::slice::from_ref(name),
                &fallback_sources,
            ) {
                resolved.extend(values);
            }
        }
    }
    Ok(resolved)
}

fn runner_exec_secret_env_names(
    command: &[String],
    preflight: Option<&RunnerCapabilityPreflight>,
    explicit_names: &[String],
) -> Vec<String> {
    let mut names = Vec::new();
    names.extend(explicit_names.iter().cloned());
    if let Some(preflight) = preflight {
        names.extend(preflight.required_env.iter().cloned());
    }
    names.extend(super::lab::secrets::declared_agent_task_secret_env(command));
    names.extend(super::lab::secrets::declared_trace_secret_env(command));
    names.extend(super::lab::secrets::declared_tunnel_secret_env(command));
    names.sort();
    names.dedup();
    names
}

fn validate_required_paths(
    runner: &Runner,
    required_paths: &[String],
    validate_on_host: bool,
) -> Result<()> {
    for path in required_paths {
        if !Path::new(path).is_absolute() {
            return Err(Error::validation_invalid_argument(
                "require_path",
                "runner exec --require-path expects absolute paths on the runner",
                Some(path.to_string()),
                Some(vec![
                    "Pass the path as it exists on the runner, not the controller.".to_string(),
                ]),
            ));
        }
        if (validate_on_host || runner.kind == RunnerKind::Local) && !Path::new(path).exists() {
            return Err(missing_required_path_error(runner, path));
        }
    }

    Ok(())
}

fn validate_remote_required_paths(client: &mut SshClient, required_paths: &[String]) -> Result<()> {
    for path in required_paths {
        let output = client.execute(&format!("test -e {}", shell::quote_arg(path)));
        if output.exit_code != 0 {
            return Err(Error::validation_invalid_argument(
                "require_path",
                "required runner path does not exist",
                Some(path.to_string()),
                Some(vec![
                    "Use the generated _lab_workspaces/... snapshot path when the controller worktree path was synced into a lab snapshot.".to_string(),
                    "Run an explicit workspace sync/adopt step before referencing controller worktree paths on the runner.".to_string(),
                ]),
            ));
        }
    }

    Ok(())
}

fn missing_required_path_error(runner: &Runner, path: &str) -> Error {
    Error::validation_invalid_argument(
        "require_path",
        "required runner path does not exist",
        Some(path.to_string()),
        Some(vec![
            format!(
                "Runner `{}` workspace_root is {}.",
                runner.id,
                runner.workspace_root.as_deref().unwrap_or("not configured")
            ),
            "Use the generated _lab_workspaces/... snapshot path when the controller worktree path was synced into a lab snapshot.".to_string(),
            "Run an explicit workspace sync/adopt step before referencing controller worktree paths on the runner.".to_string(),
        ]),
    )
}

fn validate_runner_process_cwd(runner: &Runner, cwd: &str) -> Result<()> {
    if !Path::new(cwd).is_absolute() {
        return Err(Error::validation_invalid_argument(
            "cwd",
            "runner exec requires an absolute cwd",
            Some(cwd.to_string()),
            None,
        ));
    }

    if runner.kind == RunnerKind::Local && !Path::new(cwd).is_dir() {
        return Err(Error::validation_invalid_argument(
            "cwd",
            "local runner cwd must exist and be a directory",
            Some(cwd.to_string()),
            None,
        ));
    }

    Ok(())
}

pub(crate) fn execute_runner_process(plan: &PreparedRunnerProcess) -> Result<ProcessOutput> {
    let mut command = std::process::Command::new(&plan.command[0]);
    command.args(&plan.command[1..]).current_dir(&plan.cwd);
    apply_runner_process_env(&mut command, plan);

    command_output(&mut command)
}

pub(crate) fn execute_runner_process_until_cancelled(
    plan: &PreparedRunnerProcess,
    is_cancelled: impl FnMut() -> bool,
) -> Result<ProcessOutput> {
    let mut command = std::process::Command::new(&plan.command[0]);
    command.args(&plan.command[1..]).current_dir(&plan.cwd);
    apply_runner_process_env(&mut command, plan);

    command_output_until_cancelled(&mut command, is_cancelled)
}

fn apply_runner_process_env(command: &mut std::process::Command, plan: &PreparedRunnerProcess) {
    command.env_clear();
    for key in inherited_runner_process_env_keys() {
        if !plan.env.contains_key(*key) {
            if let Some(value) = std::env::var_os(key) {
                command.env(key, value);
            }
        }
    }
    command.envs(plan.env.iter()).env(
        crate::core::observation::SOURCE_SNAPSHOT_METADATA_ENV,
        serde_json::to_string(&plan.source_snapshot).unwrap_or_default(),
    );
}

fn inherited_runner_process_env_keys() -> &'static [&'static str] {
    &["HOME", "USER", "LOGNAME", "SHELL", "TMPDIR", "TEMP", "TMP"]
}

fn command_output(command: &mut std::process::Command) -> Result<ProcessOutput> {
    let measured = measured_command_output(command)?;
    let output = measured.output;
    Ok(ProcessOutput {
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        exit_code: output.status.code().unwrap_or(1),
        metrics: Some(measured.metrics),
        capture: Some(measured.capture),
    })
}

fn command_output_until_cancelled(
    command: &mut std::process::Command,
    is_cancelled: impl FnMut() -> bool,
) -> Result<ProcessOutput> {
    let measured = measured_command_output_until_cancelled(command, is_cancelled)?;
    let output = measured.output;
    Ok(ProcessOutput {
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        exit_code: output.status.code().unwrap_or(1),
        metrics: Some(measured.metrics),
        capture: Some(measured.capture),
    })
}

fn exec_output(
    runner: &Runner,
    mode: RunnerExecMode,
    cwd: String,
    command: Vec<String>,
    output: ProcessOutput,
    source_snapshot: Option<SourceSnapshot>,
    require_paths: Vec<String>,
    redaction_env: &HashMap<String, String>,
    secret_env_names: &[String],
) -> (RunnerExecOutput, i32) {
    let exit_code = output.exit_code;
    let (stdout, stderr) = redact_runner_exec_streams(
        output.stdout,
        output.stderr,
        redaction_env,
        secret_env_names,
    );
    (
        RunnerExecOutput {
            command: "runner.exec",
            runner_id: runner.id.clone(),
            dry_run: false,
            mode,
            argv: command,
            remote_cwd: cwd,
            exit_code,
            stdout,
            stderr,
            source_snapshot: source_snapshot.clone(),
            job: None,
            job_id: None,
            job_events: None,
            mirror_run_id: None,
            patch: None,
            metrics: output.metrics,
            capture: output.capture,
            diagnostics: runner_exec_diagnostics(runner, source_snapshot.as_ref(), &require_paths),
        },
        exit_code,
    )
}

fn redact_runner_exec_streams(
    stdout: String,
    stderr: String,
    env: &HashMap<String, String>,
    secret_env_names: &[String],
) -> (String, String) {
    let policy = RedactionPolicy::default();
    let secrets = runner_exec_secret_values(env, secret_env_names, &policy);
    (
        redact_runner_exec_text(&stdout, &policy, &secrets),
        redact_runner_exec_text(&stderr, &policy, &secrets),
    )
}

fn runner_exec_secret_values(
    env: &HashMap<String, String>,
    secret_env_names: &[String],
    policy: &RedactionPolicy,
) -> Vec<String> {
    let mut values = env
        .iter()
        .filter_map(|(key, value)| {
            if value.is_empty() {
                return None;
            }
            if policy.is_sensitive_key(key) || secret_env_names.iter().any(|name| name == key) {
                Some(value.clone())
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    values.sort_by_key(|value| std::cmp::Reverse(value.len()));
    values.dedup();
    values
}

fn redact_runner_exec_text(
    text: &str,
    policy: &RedactionPolicy,
    secret_values: &[String],
) -> String {
    let mut redacted = policy.redact_string(text);
    for value in secret_values {
        redacted = redacted.replace(value, policy.replacement());
    }
    redacted
}

fn redact_runner_job_events(
    events: &[JobEvent],
    env: &HashMap<String, String>,
    secret_env_names: &[String],
) -> Vec<JobEvent> {
    let policy = RedactionPolicy::default();
    let secrets = runner_exec_secret_values(env, secret_env_names, &policy);
    events
        .iter()
        .map(|event| {
            let mut redacted = event.clone();
            redacted.message = redacted
                .message
                .as_deref()
                .map(|message| redact_runner_exec_text(message, &policy, &secrets));
            redacted.data = redacted
                .data
                .as_ref()
                .map(|data| redact_runner_exec_json(data, &policy, &secrets));
            redacted
        })
        .collect()
}

fn redact_runner_exec_json(
    value: &Value,
    policy: &RedactionPolicy,
    secret_values: &[String],
) -> Value {
    match policy.redact_json(value) {
        Value::String(text) => Value::String(redact_runner_exec_text(&text, policy, secret_values)),
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| redact_runner_exec_json(item, policy, secret_values))
                .collect(),
        ),
        Value::Object(object) => Value::Object(
            object
                .iter()
                .map(|(key, value)| {
                    (
                        key.clone(),
                        redact_runner_exec_json(value, policy, secret_values),
                    )
                })
                .collect(),
        ),
        redacted => redacted,
    }
}

fn runner_exec_diagnostics(
    runner: &Runner,
    source_snapshot: Option<&SourceSnapshot>,
    required_paths: &[String],
) -> Option<RunnerExecDiagnostics> {
    if required_paths.is_empty() {
        return None;
    }

    Some(RunnerExecDiagnostics {
        runner_workspace_root: runner.workspace_root.clone(),
        source_snapshot_remote_path: source_snapshot.and_then(|snapshot| snapshot.remote_path.clone()),
        required_paths: required_paths.to_vec(),
        hints: vec![
            "Use the generated _lab_workspaces/... snapshot path when the controller worktree path was synced into a lab snapshot.".to_string(),
            "Use --require-path to preflight paths that a command will reference before running it.".to_string(),
        ],
    })
}

fn resolve_cwd(runner: &Runner, cwd: Option<&str>) -> Result<String> {
    match runner.kind {
        RunnerKind::Local => {
            if let Some(cwd) = cwd {
                return Ok(cwd.to_string());
            }
            if let Some(root) = &runner.workspace_root {
                return Ok(root.clone());
            }
            std::env::current_dir()
                .map(|path| path.display().to_string())
                .map_err(|err| {
                    Error::internal_io(err.to_string(), Some("read current directory".to_string()))
                })
        }
        RunnerKind::Ssh => {
            let Some(root) = runner.workspace_root.as_deref() else {
                return Err(Error::validation_invalid_argument(
                    "workspace_root",
                    "SSH runner execution requires workspace_root so local paths are not silently reused remotely",
                    Some(runner.id.clone()),
                    Some(vec!["Set the runner workspace root or pass --cwd inside that root.".to_string()]),
                ));
            };
            let remote_cwd = cwd.unwrap_or(root);
            validate_remote_cwd(root, remote_cwd)?;
            Ok(remote_cwd.to_string())
        }
    }
}

fn validate_remote_cwd(root: &str, cwd: &str) -> Result<()> {
    if !root.starts_with('/') || !cwd.starts_with('/') {
        return Err(Error::validation_invalid_argument(
            "cwd",
            "remote runner cwd and workspace_root must be absolute paths",
            Some(cwd.to_string()),
            None,
        ));
    }
    let root = trim_trailing_slashes(root);
    let cwd = trim_trailing_slashes(cwd);
    if cwd == root || cwd.starts_with(&format!("{root}/")) {
        return Ok(());
    }
    Err(Error::validation_invalid_argument(
        "cwd",
        "remote cwd must be inside the configured runner workspace_root",
        Some(cwd),
        Some(vec![format!("Use a path under {root}")]),
    ))
}

fn trim_trailing_slashes(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        "/".to_string()
    } else {
        trimmed.to_string()
    }
}

fn string_field(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn first_non_empty_line(value: &str) -> Option<&str> {
    value.lines().map(str::trim).find(|line| !line.is_empty())
}

fn find_runner_homeboy_error(output: &RunnerExecOutput) -> Option<Value> {
    find_homeboy_error_in_text(&output.stdout)
        .or_else(|| find_homeboy_error_in_text(&output.stderr))
        .or_else(|| {
            output.job_events.as_ref().and_then(|events| {
                events.iter().find_map(|event| {
                    event
                        .message
                        .as_deref()
                        .and_then(find_homeboy_error_in_text)
                        .or_else(|| event.data.as_ref().and_then(homeboy_error_from_envelope))
                })
            })
        })
}

fn find_homeboy_error_in_text(text: &str) -> Option<Value> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    parse_homeboy_error_json(trimmed)
        .or_else(|| {
            trimmed
                .lines()
                .find_map(|line| parse_homeboy_error_json(line.trim()))
        })
        .or_else(|| {
            let start = trimmed.find('{')?;
            let end = trimmed.rfind('}')?;
            if end <= start {
                return None;
            }
            parse_homeboy_error_json(&trimmed[start..=end])
        })
}

fn parse_homeboy_error_json(candidate: &str) -> Option<Value> {
    serde_json::from_str::<Value>(candidate)
        .ok()
        .and_then(|value| homeboy_error_from_envelope(&value))
}

fn homeboy_error_from_envelope(value: &Value) -> Option<Value> {
    if value.get("success").and_then(Value::as_bool) != Some(false) {
        return None;
    }
    let error = value.get("error")?;
    if error.is_null() {
        return None;
    }
    if error.is_object() {
        return Some(error.clone());
    }
    Some(json!({ "message": error }))
}

/// Returns the structured failure detail for a daemon exec response, or `None`
/// when the daemon answered without any usable error/data payload (the classic
/// stale-or-restarting daemon signature behind the historical
/// `daemon exec request failed: null` symptom in #3631 / #3624).
fn daemon_failure_payload_message(envelope: &DaemonEnvelope) -> Option<String> {
    let payload = envelope
        .error
        .as_ref()
        .or(envelope.data.as_ref())
        .filter(|value| !value.is_null())?;

    let code = payload.get("error").and_then(Value::as_str);
    let message = payload.get("message").and_then(Value::as_str);
    Some(match (code, message) {
        (Some(code), Some(message)) => format!("{code}: {message}"),
        (Some(code), None) => code.to_string(),
        (None, Some(message)) => message.to_string(),
        (None, None) => payload.to_string(),
    })
}

/// Build the controller-facing error for a daemon exec submission that came back
/// with a failure envelope. When the daemon returned no usable error payload we
/// treat it as a stale/restarting daemon and surface reconnect guidance instead
/// of the historical opaque `null` (#3631, #3624).
fn daemon_exec_request_failed_error(
    runner_id: &str,
    status_code: u16,
    envelope: &DaemonEnvelope,
) -> Error {
    match daemon_failure_payload_message(envelope) {
        Some(detail) => Error::internal_unexpected(format!(
            "daemon exec request failed: {detail}"
        ))
        .with_hint(format!(
            "Runner `{runner_id}` daemon rejected the exec request (HTTP {status_code})."
        )),
        None => Error::internal_unexpected(format!(
            "runner `{runner_id}` daemon returned no result for the exec request (HTTP {status_code} with an empty error payload); the daemon is likely stale or was restarted"
        ))
        .with_hint(format!(
            "Reconnect the runner with `homeboy runner disconnect {runner_id} && homeboy runner connect {runner_id}`, then retry. If it persists, kill any stale daemon with `homeboy runner doctor {runner_id}`."
        ))
        .with_hint(
            "A daemon that reports SSH-healthy can still serve a stale process; reconnecting rebinds the tunnel to the live daemon.".to_string(),
        ),
    }
}

/// The exec POST itself failed at the transport layer (connection refused /
/// reset while the daemon restarts, tunnel torn down, etc.). Surface a clear
/// reconnect path rather than a bare reqwest error string (#3631, #3624).
fn daemon_exec_transport_error(runner_id: &str, err: reqwest::Error) -> Error {
    Error::internal_unexpected(format!(
        "could not reach runner `{runner_id}` daemon to submit the exec request: {err}"
    ))
    .with_hint(format!(
        "The daemon tunnel may be stale or the daemon may have restarted. Reconnect with `homeboy runner disconnect {runner_id} && homeboy runner connect {runner_id}` and retry."
    ))
}

/// The exec response body could not be parsed as a daemon envelope — typically a
/// stale daemon answering with an empty or non-JSON body (#3631, #3624).
fn daemon_exec_stale_response_error(runner_id: &str, status_code: u16, parse_err: &str) -> Error {
    Error::internal_unexpected(format!(
        "runner `{runner_id}` daemon returned an unreadable exec response (HTTP {status_code}): {parse_err}; the daemon is likely stale or was restarted mid-request"
    ))
    .with_hint(format!(
        "Reconnect with `homeboy runner disconnect {runner_id} && homeboy runner connect {runner_id}` and retry; if a stale daemon PID lingers, run `homeboy runner doctor {runner_id}`."
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::defaults::AgentTaskSecretSource;
    use crate::core::error::ErrorCode;
    use crate::core::server::{self, RunnerPolicy, RunnerSecretEnvRef, RunnerSettings};

    fn ssh_runner() -> Runner {
        Runner {
            id: "lab".to_string(),
            kind: RunnerKind::Ssh,
            server_id: Some("srv".to_string()),
            workspace_root: Some("/srv/homeboy".to_string()),
            settings: RunnerSettings {
                daemon: true,
                ..Default::default()
            },
            env: Default::default(),
            secret_env: Default::default(),
            resources: Default::default(),
            policy: RunnerPolicy::default(),
        }
    }

    fn local_runner(workspace_root: String) -> Runner {
        Runner {
            id: "local".to_string(),
            kind: RunnerKind::Local,
            server_id: None,
            workspace_root: Some(workspace_root),
            settings: RunnerSettings::default(),
            env: Default::default(),
            secret_env: Default::default(),
            resources: Default::default(),
            policy: RunnerPolicy::default(),
        }
    }

    fn failed_runner_exec_output(stdout: &str, stderr: &str) -> RunnerExecOutput {
        RunnerExecOutput {
            command: "runner.exec",
            runner_id: "lab".to_string(),
            dry_run: false,
            mode: RunnerExecMode::Daemon,
            argv: vec![
                "homeboy".to_string(),
                "extension".to_string(),
                "install".to_string(),
            ],
            remote_cwd: "/srv/homeboy/project".to_string(),
            exit_code: 2,
            stdout: stdout.to_string(),
            stderr: stderr.to_string(),
            source_snapshot: None,
            job: None,
            job_id: Some("job-123".to_string()),
            job_events: None,
            mirror_run_id: None,
            patch: None,
            metrics: None,
            capture: None,
            diagnostics: None,
        }
    }

    #[test]
    fn runner_exec_redacts_env_diagnostic_assignments() {
        let env = HashMap::from([(
            "HOMEBOY_PREVIEW_TUNNEL_TOKEN".to_string(),
            "preview-token-secret".to_string(),
        )]);

        let (stdout, stderr) = redact_runner_exec_streams(
            "HOMEBOY_PREVIEW_TUNNEL_TOKEN=preview-token-secret\nSAFE=value\n".to_string(),
            "token=preview-token-secret\n".to_string(),
            &env,
            &[],
        );

        assert_eq!(
            stdout,
            "HOMEBOY_PREVIEW_TUNNEL_TOKEN=[REDACTED]\nSAFE=value\n"
        );
        assert_eq!(stderr, "token=[REDACTED]\n");
    }

    #[test]
    fn runner_exec_redacts_bare_secret_values() {
        let env = HashMap::from([(
            "OPENAI_API_KEY".to_string(),
            "sk-test-secret-value".to_string(),
        )]);

        let (stdout, stderr) = redact_runner_exec_streams(
            "sk-test-secret-value\n".to_string(),
            "failed with sk-test-secret-value".to_string(),
            &env,
            &[],
        );

        assert_eq!(stdout, "[REDACTED]\n");
        assert_eq!(stderr, "failed with [REDACTED]");
    }

    #[test]
    fn runner_exec_redacts_daemon_job_events() {
        let env = HashMap::from([(
            "HOMEBOY_PREVIEW_TUNNEL_TOKEN".to_string(),
            "preview-token-secret".to_string(),
        )]);
        let events = vec![crate::core::api_jobs::JobEvent {
            sequence: 1,
            job_id: uuid::Uuid::new_v4(),
            kind: crate::core::api_jobs::JobEventKind::Result,
            timestamp_ms: 1,
            message: Some("HOMEBOY_PREVIEW_TUNNEL_TOKEN=preview-token-secret".to_string()),
            data: Some(json!({
                "stdout": "preview-token-secret",
                "stderr": "token=preview-token-secret",
            })),
        }];

        let redacted = redact_runner_job_events(&events, &env, &[]);

        assert_eq!(
            redacted[0].message.as_deref(),
            Some("HOMEBOY_PREVIEW_TUNNEL_TOKEN=[REDACTED]")
        );
        assert_eq!(redacted[0].data.as_ref().unwrap()["stdout"], "[REDACTED]");
        assert_eq!(
            redacted[0].data.as_ref().unwrap()["stderr"],
            "token=[REDACTED]"
        );
    }

    #[test]
    fn runner_exec_failure_error_promotes_homeboy_stdout_error() {
        let output = failed_runner_exec_output(
            r#"{"success":false,"error":{"code":"validation.invalid_argument","message":"Invalid argument 'source': Path does not exist: /Users/user/Developer/homeboy-extensions/wordpress","details":{"field":"source"}}}"#,
            "",
        );

        let err = runner_exec_failure_error(&output).expect("runner failure error");

        assert_eq!(err.code, ErrorCode::RemoteCommandFailed);
        assert!(err.message.contains("Path does not exist"));
        assert_eq!(
            err.details["runner_error"]["code"].as_str(),
            Some("validation.invalid_argument")
        );
        assert_eq!(err.details["runner_id"].as_str(), Some("lab"));
        assert_eq!(err.details["job_id"].as_str(), Some("job-123"));
        assert_eq!(
            err.details["remote_cwd"].as_str(),
            Some("/srv/homeboy/project")
        );
        assert_eq!(err.details["exit_code"].as_i64(), Some(2));
        assert_eq!(
            err.details["execution"]["stdout"].as_str(),
            Some(output.stdout.as_str())
        );
    }

    #[test]
    fn runner_exec_failure_error_promotes_homeboy_job_event_message_error() {
        let mut output = failed_runner_exec_output("", "generic stderr");
        output.job_events = Some(vec![crate::core::api_jobs::JobEvent {
            sequence: 1,
            job_id: uuid::Uuid::new_v4(),
            kind: crate::core::api_jobs::JobEventKind::Error,
            timestamp_ms: 1,
            message: Some(
                r#"runner emitted: {"success":false,"error":{"code":"extension.not_found","message":"Extension not found: wordpress"}}"#
                    .to_string(),
            ),
            data: None,
        }]);

        let err = runner_exec_failure_error(&output).expect("runner failure error");

        assert!(err.message.contains("Extension not found: wordpress"));
        assert_eq!(
            err.details["runner_error"]["code"].as_str(),
            Some("extension.not_found")
        );
        assert!(err.details["execution"]["job_events"].is_array());
    }

    fn policy_request(options: &RunnerExecOptions) -> RunnerPolicyRequest<'_> {
        RunnerPolicyRequest {
            project_id: options.project_id.as_deref(),
            command: &options.command,
            capture_patch: options.capture_patch,
            raw_exec: options.raw_exec,
        }
    }

    struct EnvVarGuard {
        name: &'static str,
        prior: Option<String>,
    }

    impl EnvVarGuard {
        fn set(name: &'static str, value: &str) -> Self {
            let prior = std::env::var(name).ok();
            std::env::set_var(name, value);
            Self { name, prior }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match &self.prior {
                Some(value) => std::env::set_var(self.name, value),
                None => std::env::remove_var(self.name),
            }
        }
    }

    fn json_file_source(path: &str, field: &str) -> AgentTaskSecretSource {
        AgentTaskSecretSource {
            source: "json-file".to_string(),
            env_var: None,
            path: Some(path.to_string()),
            scope: None,
            name: None,
            field: Some(field.to_string()),
            value: None,
        }
    }

    #[test]
    fn provider_file_secret_source_provisions_group_json_file_sources_without_values() {
        let sources = HashMap::from([
            (
                "PROVIDER_ACCESS_TOKEN".to_string(),
                json_file_source("~/.provider/auth.json", "tokens.access_token"),
            ),
            (
                "PROVIDER_REFRESH_TOKEN".to_string(),
                json_file_source("~/.provider/auth.json", "tokens.refresh_token"),
            ),
            (
                "UNRELATED_SECRET".to_string(),
                AgentTaskSecretSource {
                    source: "env".to_string(),
                    env_var: Some("UNRELATED_SECRET".to_string()),
                    path: None,
                    scope: None,
                    name: None,
                    field: None,
                    value: None,
                },
            ),
        ]);

        let provisions = provider_file_secret_source_provisions(
            &[
                "PROVIDER_REFRESH_TOKEN".to_string(),
                "PROVIDER_ACCESS_TOKEN".to_string(),
                "UNRELATED_SECRET".to_string(),
            ],
            &sources,
        );

        assert_eq!(provisions.len(), 1);
        assert_eq!(provisions[0].path, "~/.provider/auth.json");
        assert_eq!(
            provisions[0].env_names,
            vec![
                "PROVIDER_ACCESS_TOKEN".to_string(),
                "PROVIDER_REFRESH_TOKEN".to_string(),
            ]
        );
        let rendered = format!("{:?}", provisions);
        assert!(!rendered.contains("access-secret"));
        assert!(!rendered.contains("refresh-secret"));
    }

    #[test]
    fn runner_secret_env_resolution_uses_provider_json_file_source_values() {
        crate::test_support::with_isolated_home(|home| {
            let provider_dir = home.path().join(".provider");
            std::fs::create_dir_all(&provider_dir).expect("provider dir");
            std::fs::write(
                provider_dir.join("auth.json"),
                serde_json::json!({
                    "tokens": {
                        "access_token": "access-secret-value",
                        "refresh_token": "refresh-secret-value"
                    }
                })
                .to_string(),
            )
            .expect("auth json");
            let sources = HashMap::from([
                (
                    "PROVIDER_ACCESS_TOKEN".to_string(),
                    json_file_source("~/.provider/auth.json", "tokens.access_token"),
                ),
                (
                    "PROVIDER_REFRESH_TOKEN".to_string(),
                    json_file_source("~/.provider/auth.json", "tokens.refresh_token"),
                ),
            ]);

            let resolved = resolve_runner_secret_env_for_command_with_fallbacks(
                &HashMap::new(),
                &[
                    "PROVIDER_ACCESS_TOKEN".to_string(),
                    "PROVIDER_REFRESH_TOKEN".to_string(),
                ],
                &HashMap::new(),
                &sources,
            )
            .expect("provider sources resolve on runner");

            assert_eq!(
                resolved.get("PROVIDER_ACCESS_TOKEN"),
                Some(&"access-secret-value".to_string())
            );
            assert_eq!(
                resolved.get("PROVIDER_REFRESH_TOKEN"),
                Some(&"refresh-secret-value".to_string())
            );
        });
    }

    #[test]
    fn provider_file_secret_source_error_is_early_clear_and_redacted() {
        let provision = ProviderFileSecretSourceProvision {
            path: "~/.provider/auth.json".to_string(),
            env_names: vec![
                "PROVIDER_ACCESS_TOKEN".to_string(),
                "PROVIDER_REFRESH_TOKEN".to_string(),
            ],
        };

        let err = provider_file_secret_source_error(
            "homeboy-lab",
            &provision,
            "controller credential source is not readable".to_string(),
        );

        assert_eq!(err.code, ErrorCode::ValidationInvalidArgument);
        assert!(err.message.contains("homeboy-lab"));
        assert!(err.message.contains("PROVIDER_ACCESS_TOKEN"));
        assert!(err
            .message
            .contains("controller credential source is not readable"));
        assert!(err.details["tried"]
            .as_array()
            .is_some_and(|hints| hints.iter().any(|hint| hint
                .as_str()
                .is_some_and(|hint| hint.contains("Refresh the provider credentials")))));
        let rendered = format!("{} {:?} {:?}", err.message, err.details, err.hints);
        assert!(!rendered.contains("access-secret-value"));
        assert!(!rendered.contains("refresh-secret-value"));
    }

    #[test]
    fn daemon_job_context_error_preserves_in_flight_job_details() {
        let source = Error::internal_unexpected(
            "query runner daemon: error sending request for url (http://127.0.0.1:63203/jobs/job-123)",
        )
        .with_hint("original hint");

        let err = daemon_job_context_error("homeboy-lab", "job-123", source);

        assert_eq!(err.code, ErrorCode::InternalUnexpected);
        assert_eq!(err.retryable, Some(true));
        assert_eq!(err.details["runner_id"], "homeboy-lab");
        assert_eq!(err.details["job_id"], "job-123");
        assert_eq!(err.hints[0].message, "original hint");
        assert!(err.message.contains("query runner daemon"));
    }

    #[test]
    fn test_resolve_cwd_defaults_ssh_runner_to_workspace_root() {
        let cwd = resolve_cwd(&ssh_runner(), None).expect("cwd");
        assert_eq!(cwd, "/srv/homeboy");
    }

    #[test]
    fn test_resolve_cwd_rejects_ssh_cwd_outside_workspace_root() {
        let err = resolve_cwd(&ssh_runner(), Some("/tmp/project")).expect_err("reject cwd");
        assert_eq!(err.code.as_str(), "validation.invalid_argument");
        assert!(err.message.contains("workspace_root"));
    }

    #[test]
    fn prepare_runner_process_uses_embedded_runner_snapshot() {
        crate::test_support::with_isolated_home(|_| {
            let plan = prepare_runner_process(RunnerProcessRequest {
                runner_id: "lab".to_string(),
                runner: Some(ssh_runner()),
                cwd: Some("/srv/homeboy/project".to_string()),
                project_id: None,
                command: vec!["homeboy".to_string(), "--version".to_string()],
                env: Default::default(),
                secret_env_names: Vec::new(),
                capture_patch: false,
                raw_exec: false,
                source_snapshot: None,
                require_paths: Vec::new(),
                validate_require_paths_on_host: false,
            })
            .expect("prepare from runner snapshot");

            assert_eq!(plan.runner.id, "lab");
            assert_eq!(plan.cwd, "/srv/homeboy/project");
        });
    }

    #[test]
    fn ssh_runner_prep_leaves_default_path_to_runner_side() {
        crate::test_support::with_isolated_home(|_| {
            let plan = prepare_runner_process(RunnerProcessRequest {
                runner_id: "lab".to_string(),
                runner: Some(ssh_runner()),
                cwd: Some("/srv/homeboy/project".to_string()),
                project_id: None,
                command: vec!["node".to_string(), "--version".to_string()],
                env: Default::default(),
                secret_env_names: Vec::new(),
                capture_patch: false,
                raw_exec: false,
                source_snapshot: None,
                require_paths: Vec::new(),
                validate_require_paths_on_host: false,
            })
            .expect("prepare ssh runner process");

            assert!(
                !plan.env.contains_key("PATH"),
                "controller must not freeze PATH before daemon-side runner normalization"
            );
        });
    }

    #[test]
    fn ssh_runner_prep_preserves_explicit_path() {
        crate::test_support::with_isolated_home(|_| {
            let mut runner = ssh_runner();
            runner
                .env
                .insert("PATH".to_string(), "$HOME/custom/bin:$PATH".to_string());

            let plan = prepare_runner_process(RunnerProcessRequest {
                runner_id: "lab".to_string(),
                runner: Some(runner),
                cwd: Some("/srv/homeboy/project".to_string()),
                project_id: None,
                command: vec!["node".to_string(), "--version".to_string()],
                env: Default::default(),
                secret_env_names: Vec::new(),
                capture_patch: false,
                raw_exec: false,
                source_snapshot: None,
                require_paths: Vec::new(),
                validate_require_paths_on_host: false,
            })
            .expect("prepare ssh runner process with explicit path");

            assert_eq!(
                plan.env.get("PATH").map(String::as_str),
                Some("$HOME/custom/bin:$PATH")
            );
        });
    }

    #[test]
    fn ssh_runner_prep_marks_commands_as_runner_hosted() {
        crate::test_support::with_isolated_home(|_| {
            let plan = prepare_runner_process(RunnerProcessRequest {
                runner_id: "lab".to_string(),
                runner: Some(ssh_runner()),
                cwd: Some("/srv/homeboy/project".to_string()),
                project_id: None,
                command: vec![
                    "homeboy".to_string(),
                    "agent-task".to_string(),
                    "cook".to_string(),
                ],
                env: Default::default(),
                secret_env_names: Vec::new(),
                capture_patch: false,
                raw_exec: false,
                source_snapshot: None,
                require_paths: Vec::new(),
                validate_require_paths_on_host: false,
            })
            .expect("prepare ssh runner process");

            assert_eq!(
                plan.env.get(RUNNER_HOSTED_EXEC_ENV).map(String::as_str),
                Some("1")
            );
        });
    }

    #[test]
    fn local_runner_prep_does_not_mark_commands_as_runner_hosted() {
        crate::test_support::with_isolated_home(|_| {
            let temp = tempfile::tempdir().expect("tempdir");
            let workspace = temp.path().join("project");
            std::fs::create_dir_all(&workspace).expect("workspace");

            let plan = prepare_runner_process(RunnerProcessRequest {
                runner_id: "local".to_string(),
                runner: Some(local_runner(workspace.display().to_string())),
                cwd: Some(workspace.display().to_string()),
                project_id: None,
                command: vec![
                    "homeboy".to_string(),
                    "agent-task".to_string(),
                    "cook".to_string(),
                ],
                env: Default::default(),
                secret_env_names: Vec::new(),
                capture_patch: false,
                raw_exec: false,
                source_snapshot: None,
                require_paths: Vec::new(),
                validate_require_paths_on_host: false,
            })
            .expect("prepare local runner process");

            assert!(!plan.env.contains_key(RUNNER_HOSTED_EXEC_ENV));
        });
    }

    #[test]
    fn daemon_local_prep_normalizes_default_path_on_runner_side() {
        crate::test_support::with_isolated_home(|_| {
            let temp = tempfile::tempdir().expect("tempdir");
            let workspace = temp.path().join("project");
            std::fs::create_dir_all(&workspace).expect("workspace");
            let workspace = workspace.display().to_string();

            let plan = prepare_daemon_local_process(RunnerProcessRequest {
                runner_id: "lab".to_string(),
                runner: Some(ssh_runner()),
                cwd: Some(workspace),
                project_id: None,
                command: vec!["node".to_string(), "--version".to_string()],
                env: Default::default(),
                secret_env_names: Vec::new(),
                capture_patch: false,
                raw_exec: false,
                source_snapshot: None,
                require_paths: Vec::new(),
                validate_require_paths_on_host: false,
            })
            .expect("prepare daemon-local runner process");

            assert!(
                plan.env.contains_key("PATH"),
                "daemon-side runner prep should build the default job PATH from the runner host"
            );
        });
    }

    #[test]
    fn remote_daemon_secret_env_refs_forward_controller_secrets_and_keep_runner_refs_local() {
        crate::test_support::with_isolated_home(|_| {
            let temp = tempfile::tempdir().expect("tempdir");
            let workspace = temp.path().join("workspace");
            std::fs::create_dir_all(&workspace).expect("workspace");
            let secret_file = temp.path().join("runner-secret");
            std::fs::write(&secret_file, "dummy-runner-secret\n").expect("secret file");
            crate::core::agent_task_secrets::set_config_secret(
                "HOMEBOY_CONTROLLER_SECRET_TEST_KEY",
                "dummy-controller-secret",
            )
            .expect("configure controller secret");

            let mut controller_runner = ssh_runner();
            controller_runner.workspace_root = Some(workspace.display().to_string());
            controller_runner.secret_env.insert(
                "CONTROLLER_API_KEY".to_string(),
                RunnerSecretEnvRef {
                    env: None,
                    file: None,
                    secret: Some("HOMEBOY_CONTROLLER_SECRET_TEST_KEY".to_string()),
                },
            );
            controller_runner.secret_env.insert(
                "RUNNER_API_KEY".to_string(),
                RunnerSecretEnvRef {
                    env: None,
                    file: Some(secret_file.display().to_string()),
                    secret: None,
                },
            );

            let controller_plan = prepare_runner_process(RunnerProcessRequest {
                runner_id: "lab".to_string(),
                runner: Some(controller_runner),
                cwd: Some(workspace.display().to_string()),
                project_id: None,
                command: vec!["true".to_string()],
                env: Default::default(),
                secret_env_names: vec![
                    "CONTROLLER_API_KEY".to_string(),
                    "RUNNER_API_KEY".to_string(),
                ],
                capture_patch: false,
                raw_exec: false,
                source_snapshot: Some(SourceSnapshot::existing_remote(
                    "lab",
                    &workspace.display().to_string(),
                    Some(&workspace.display().to_string()),
                )),
                require_paths: Vec::new(),
                validate_require_paths_on_host: false,
            })
            .expect("controller prep forwards configured secret refs for SSH runner");

            assert_eq!(
                controller_plan
                    .env
                    .get("CONTROLLER_API_KEY")
                    .map(String::as_str),
                Some("dummy-controller-secret")
            );
            assert!(!controller_plan.env.contains_key("RUNNER_API_KEY"));

            let mut daemon_runner = ssh_runner();
            daemon_runner.workspace_root = Some(workspace.display().to_string());
            daemon_runner.secret_env.insert(
                "RUNNER_API_KEY".to_string(),
                RunnerSecretEnvRef {
                    env: None,
                    file: Some(secret_file.display().to_string()),
                    secret: None,
                },
            );

            let daemon_plan = prepare_daemon_local_process(RunnerProcessRequest {
                runner_id: "lab".to_string(),
                runner: Some(daemon_runner),
                cwd: Some(workspace.display().to_string()),
                project_id: None,
                command: vec![
                    "homeboy".to_string(),
                    "trace".to_string(),
                    "compare".to_string(),
                    "demo".to_string(),
                    "scenario".to_string(),
                    "--secret-env=RUNNER_API_KEY".to_string(),
                ],
                env: Default::default(),
                secret_env_names: vec!["RUNNER_API_KEY".to_string()],
                capture_patch: false,
                raw_exec: false,
                source_snapshot: Some(SourceSnapshot::existing_remote(
                    "lab",
                    &workspace.display().to_string(),
                    Some(&workspace.display().to_string()),
                )),
                require_paths: Vec::new(),
                validate_require_paths_on_host: true,
            })
            .expect("daemon prep resolves secret refs on runner side");

            assert_eq!(
                daemon_plan.env.get("RUNNER_API_KEY").map(String::as_str),
                Some("dummy-runner-secret")
            );
        });
    }

    #[test]
    fn daemon_read_only_runner_exec_ignores_unrelated_missing_secret_env_refs() {
        crate::test_support::with_isolated_home(|_| {
            let temp = tempfile::tempdir().expect("tempdir");
            let workspace = temp.path().join("workspace");
            std::fs::create_dir_all(&workspace).expect("workspace");

            let mut runner = ssh_runner();
            runner.workspace_root = Some(workspace.display().to_string());
            runner.secret_env.insert(
                "HOMEBOY_PREVIEW_TUNNEL_TOKEN".to_string(),
                RunnerSecretEnvRef {
                    env: Some("HOMEBOY_PREVIEW_TUNNEL_TOKEN".to_string()),
                    file: None,
                    secret: None,
                },
            );
            std::env::remove_var("HOMEBOY_PREVIEW_TUNNEL_TOKEN");

            let plan = prepare_daemon_local_process(RunnerProcessRequest {
                runner_id: "lab".to_string(),
                runner: Some(runner),
                cwd: Some(workspace.display().to_string()),
                project_id: None,
                command: vec![
                    "/bin/ps".to_string(),
                    "-eo".to_string(),
                    "pid,ppid,etime,stat,pcpu,pmem,cmd".to_string(),
                ],
                env: Default::default(),
                secret_env_names: Vec::new(),
                capture_patch: false,
                raw_exec: true,
                source_snapshot: Some(SourceSnapshot::existing_remote(
                    "lab",
                    &workspace.display().to_string(),
                    Some(&workspace.display().to_string()),
                )),
                require_paths: Vec::new(),
                validate_require_paths_on_host: true,
            })
            .expect("read-only runner exec ignores unrelated optional secret refs");

            assert!(!plan.env.contains_key("HOMEBOY_PREVIEW_TUNNEL_TOKEN"));
        });
    }

    #[test]
    fn daemon_runner_exec_requires_declared_missing_secret_env_refs() {
        crate::test_support::with_isolated_home(|_| {
            let temp = tempfile::tempdir().expect("tempdir");
            let workspace = temp.path().join("workspace");
            std::fs::create_dir_all(&workspace).expect("workspace");
            std::env::remove_var("HOMEBOY_REQUIRED_SECRET_TEST_KEY");

            let mut runner = ssh_runner();
            runner.workspace_root = Some(workspace.display().to_string());
            runner.secret_env.insert(
                "HOMEBOY_REQUIRED_SECRET_TEST_KEY".to_string(),
                RunnerSecretEnvRef {
                    env: Some("HOMEBOY_REQUIRED_SECRET_TEST_KEY".to_string()),
                    file: None,
                    secret: None,
                },
            );

            let err = prepare_daemon_local_process(RunnerProcessRequest {
                runner_id: "lab".to_string(),
                runner: Some(runner),
                cwd: Some(workspace.display().to_string()),
                project_id: None,
                command: vec![
                    "homeboy".to_string(),
                    "trace".to_string(),
                    "compare".to_string(),
                    "demo".to_string(),
                    "scenario".to_string(),
                    "--secret-env=HOMEBOY_REQUIRED_SECRET_TEST_KEY".to_string(),
                ],
                env: Default::default(),
                secret_env_names: vec!["HOMEBOY_REQUIRED_SECRET_TEST_KEY".to_string()],
                capture_patch: false,
                raw_exec: false,
                source_snapshot: Some(SourceSnapshot::existing_remote(
                    "lab",
                    &workspace.display().to_string(),
                    Some(&workspace.display().to_string()),
                )),
                require_paths: Vec::new(),
                validate_require_paths_on_host: true,
            })
            .expect_err("declared missing command secret should fail validation");

            assert_eq!(err.code.as_str(), "validation.invalid_argument");
            assert_eq!(err.details["field"], "secret_env");
            assert!(err.message.contains("HOMEBOY_REQUIRED_SECRET_TEST_KEY"));
        });
    }

    #[test]
    fn runner_exec_secret_env_names_include_tunnel_preview_client_token() {
        let names = runner_exec_secret_env_names(
            &[
                "homeboy".to_string(),
                "tunnel".to_string(),
                "preview-client".to_string(),
                "start".to_string(),
                "--ingress".to_string(),
                "https://preview-broker.example.test".to_string(),
                "--public-host".to_string(),
                "preview.example.test".to_string(),
                "--local-origin".to_string(),
                "http://127.0.0.1:8888".to_string(),
            ],
            None,
            &[],
        );

        assert_eq!(names, vec!["HOMEBOY_PREVIEW_TUNNEL_TOKEN".to_string()]);
    }

    #[test]
    fn test_exec_runs_local_runner_command() {
        crate::test_support::with_isolated_home(|_| {
            super::super::create(r#"{"id":"lab-local","kind":"local"}"#, false)
                .expect("create local runner");

            let (output, exit_code) = exec(
                "lab-local",
                RunnerExecOptions {
                    cwd: None,
                    project_id: None,
                    allow_diagnostic_ssh: false,
                    command: vec!["sh".to_string(), "-c".to_string(), "printf ok".to_string()],
                    env: Default::default(),
                    secret_env_names: Vec::new(),
                    capture_patch: false,
                    raw_exec: false,
                    source_snapshot: None,
                    capability_preflight: None,
                    required_extensions: Vec::new(),
                    require_paths: Vec::new(),
                },
            )
            .expect("exec local runner");

            assert_eq!(exit_code, 0);
            assert_eq!(output.runner_id, "lab-local");
            assert_eq!(output.mode, RunnerExecMode::Local);
            assert_eq!(output.stdout, "ok");
            let metrics = output.metrics.expect("local exec metrics");
            assert!(metrics.duration_ms < 60_000);
            if cfg!(target_os = "linux") {
                assert_eq!(metrics.source, "linux_procfs_process_tree");
                if metrics.sample_count > 0 {
                    assert!(metrics.peak_rss_bytes.is_some());
                    assert!(metrics.child_process_count_peak.is_some());
                }
            } else {
                assert_eq!(metrics.source, "duration_only");
                assert_eq!(metrics.sample_count, 0);
            }
            let source_snapshot = output.source_snapshot.expect("source snapshot");
            assert_eq!(source_snapshot.runner_id, "lab-local");
            assert_eq!(source_snapshot.sync_mode, "existing_remote");
            assert!(source_snapshot.snapshot_hash.starts_with("sha256:"));
            assert!(output.job_id.is_none());
        });
    }

    #[test]
    fn test_exec_does_not_leak_ambient_process_env() {
        crate::test_support::with_isolated_home(|_| {
            let _guard = EnvVarGuard::set("HOMEBOY_TEST_AMBIENT_ONLY", "leaked");
            super::super::create(r#"{"id":"lab-local","kind":"local"}"#, false)
                .expect("create local runner");

            let (output, exit_code) = exec(
                "lab-local",
                RunnerExecOptions {
                    cwd: None,
                    project_id: None,
                    allow_diagnostic_ssh: false,
                    command: vec![
                        "sh".to_string(),
                        "-c".to_string(),
                        "test -z \"${HOMEBOY_TEST_AMBIENT_ONLY+x}\" && printf isolated".to_string(),
                    ],
                    env: Default::default(),
                    secret_env_names: Vec::new(),
                    capture_patch: false,
                    raw_exec: false,
                    source_snapshot: None,
                    capability_preflight: None,
                    required_extensions: Vec::new(),
                    require_paths: Vec::new(),
                },
            )
            .expect("exec local runner");

            assert_eq!(exit_code, 0);
            assert_eq!(output.stdout, "isolated");
        });
    }

    #[test]
    fn test_exec_preserves_explicit_request_env() {
        crate::test_support::with_isolated_home(|_| {
            super::super::create(r#"{"id":"lab-local","kind":"local"}"#, false)
                .expect("create local runner");

            let (output, exit_code) = exec(
                "lab-local",
                RunnerExecOptions {
                    cwd: None,
                    project_id: None,
                    allow_diagnostic_ssh: false,
                    command: vec![
                        "sh".to_string(),
                        "-c".to_string(),
                        "printf %s \"$HOMEBOY_TEST_EXPLICIT\"".to_string(),
                    ],
                    env: HashMap::from([(
                        "HOMEBOY_TEST_EXPLICIT".to_string(),
                        "planned".to_string(),
                    )]),
                    secret_env_names: Vec::new(),
                    capture_patch: false,
                    raw_exec: false,
                    source_snapshot: None,
                    capability_preflight: None,
                    required_extensions: Vec::new(),
                    require_paths: Vec::new(),
                },
            )
            .expect("exec local runner");

            assert_eq!(exit_code, 0);
            assert_eq!(output.stdout, "planned");
        });
    }

    #[test]
    fn test_exec_rejects_missing_required_local_runner_path() {
        crate::test_support::with_isolated_home(|_| {
            let workspace = tempfile::tempdir().expect("workspace");
            super::super::create(
                &format!(
                    r#"{{"id":"lab-local","kind":"local","workspace_root":"{}"}}"#,
                    workspace.path().display()
                ),
                false,
            )
            .expect("create local runner");
            let missing = workspace.path().join("missing-worktree");

            let err = exec(
                "lab-local",
                RunnerExecOptions {
                    cwd: None,
                    project_id: None,
                    allow_diagnostic_ssh: false,
                    command: vec![
                        "sh".to_string(),
                        "-c".to_string(),
                        "printf nope".to_string(),
                    ],
                    env: Default::default(),
                    secret_env_names: Vec::new(),
                    capture_patch: false,
                    raw_exec: false,
                    source_snapshot: None,
                    capability_preflight: None,
                    required_extensions: Vec::new(),
                    require_paths: vec![missing.display().to_string()],
                },
            )
            .expect_err("missing required path rejects before command");

            assert_eq!(err.code.as_str(), "validation.invalid_argument");
            assert_eq!(err.details["field"], "require_path");
            assert!(err.message.contains("required runner path"));
            assert!(err.details["tried"].to_string().contains("_lab_workspaces"));
        });
    }

    #[test]
    fn test_exec_reports_required_path_diagnostics() {
        crate::test_support::with_isolated_home(|_| {
            let workspace = tempfile::tempdir().expect("workspace");
            let required_path = workspace.path().join("project");
            std::fs::create_dir(&required_path).expect("required path");
            super::super::create(
                &format!(
                    r#"{{"id":"lab-local","kind":"local","workspace_root":"{}"}}"#,
                    workspace.path().display()
                ),
                false,
            )
            .expect("create local runner");

            let (output, exit_code) = exec(
                "lab-local",
                RunnerExecOptions {
                    cwd: None,
                    project_id: None,
                    allow_diagnostic_ssh: false,
                    command: vec!["sh".to_string(), "-c".to_string(), "printf ok".to_string()],
                    env: Default::default(),
                    secret_env_names: Vec::new(),
                    capture_patch: false,
                    raw_exec: false,
                    source_snapshot: None,
                    capability_preflight: None,
                    required_extensions: Vec::new(),
                    require_paths: vec![required_path.display().to_string()],
                },
            )
            .expect("exec with required path");

            assert_eq!(exit_code, 0);
            let diagnostics = output.diagnostics.expect("diagnostics");
            assert_eq!(
                diagnostics.runner_workspace_root,
                Some(workspace.path().display().to_string())
            );
            assert_eq!(
                diagnostics.required_paths,
                vec![required_path.display().to_string()]
            );
            assert!(diagnostics.source_snapshot_remote_path.is_some());
            assert!(diagnostics
                .hints
                .iter()
                .any(|hint| hint.contains("_lab_workspaces")));
        });
    }

    #[test]
    fn test_exec_rejects_disconnected_ssh_runner_without_diagnostic_fallback() {
        crate::test_support::with_isolated_home(|_| {
            server::create(
                r#"{"id":"lab-server","host":"192.168.86.63","user":"user"}"#,
                false,
            )
            .expect("create server");

            super::super::create(
                r#"{"id":"lab-server","kind":"ssh","server_id":"lab-server","workspace_root":"/srv/homeboy"}"#,
                false,
            )
            .expect("create ssh runner");

            let err = exec(
                "lab-server",
                RunnerExecOptions {
                    cwd: Some("/srv/homeboy/project".to_string()),
                    project_id: None,
                    allow_diagnostic_ssh: false,
                    command: vec!["homeboy".to_string(), "test".to_string()],
                    env: Default::default(),
                    secret_env_names: Vec::new(),
                    capture_patch: false,
                    raw_exec: false,
                    source_snapshot: None,
                    capability_preflight: None,
                    required_extensions: Vec::new(),
                    require_paths: Vec::new(),
                },
            )
            .expect_err("disconnected ssh runner needs daemon or diagnostic fallback");

            assert_eq!(err.code.as_str(), "validation.invalid_argument");
            assert!(err.message.contains("connected to a daemon"));
            let tried = err.details["tried"].as_array().expect("tried details");
            assert!(tried.iter().any(|detail| detail
                .as_str()
                .is_some_and(|detail| detail.contains("job metadata"))));
        });
    }

    #[test]
    fn test_diagnostic_ssh_mode_serializes_as_diagnostic_ssh() {
        assert_eq!(
            serde_json::to_value(RunnerExecMode::DiagnosticSsh).expect("mode json"),
            json!("diagnostic_ssh")
        );
    }

    #[test]
    fn explicit_diagnostic_ssh_wins_for_ssh_runners() {
        let mut options = RunnerExecOptions {
            cwd: Some("/srv/homeboy/project".to_string()),
            project_id: None,
            allow_diagnostic_ssh: true,
            command: vec!["homeboy".to_string(), "--version".to_string()],
            env: Default::default(),
            secret_env_names: Vec::new(),
            capture_patch: false,
            raw_exec: true,
            source_snapshot: None,
            capability_preflight: None,
            required_extensions: Vec::new(),
            require_paths: Vec::new(),
        };

        assert!(should_force_diagnostic_ssh(&ssh_runner(), &options));
        options.allow_diagnostic_ssh = false;
        assert!(!should_force_diagnostic_ssh(&ssh_runner(), &options));
    }

    #[test]
    fn test_required_extensions_for_command_reads_extension_flags() {
        let command = vec![
            "homeboy".to_string(),
            "lint".to_string(),
            "--extension".to_string(),
            "rust".to_string(),
            "--extension=fixture-build".to_string(),
        ];

        assert_eq!(
            required_extensions_for_command(&command, &["wordpress".to_string()]),
            vec![
                "wordpress".to_string(),
                "rust".to_string(),
                "fixture-build".to_string(),
            ]
        );
    }

    #[test]
    fn test_runner_policy_denies_raw_ssh_exec_by_default() {
        let runner = ssh_runner();
        let options = RunnerExecOptions {
            cwd: Some("/srv/homeboy/project".to_string()),
            project_id: Some("extrachill".to_string()),
            allow_diagnostic_ssh: true,
            command: vec!["sh".to_string()],
            env: Default::default(),
            secret_env_names: Vec::new(),
            capture_patch: false,
            raw_exec: true,
            source_snapshot: None,
            capability_preflight: None,
            required_extensions: Vec::new(),
            require_paths: Vec::new(),
        };

        let err = validate_runner_policy(&runner, "/srv/homeboy/project", policy_request(&options))
            .expect_err("deny raw exec");

        assert_eq!(err.code.as_str(), "runner.policy_denied");
        assert!(err.message.contains("raw exec is denied by default"));
    }

    #[test]
    fn test_runner_policy_enforces_projects_commands_workspace_and_artifacts() {
        let mut runner = ssh_runner();
        runner.policy = RunnerPolicy {
            allow_raw_exec: Some(true),
            allowed_projects: vec!["extrachill".to_string()],
            allowed_commands: vec!["cargo".to_string()],
            workspace_roots: vec!["/srv/homeboy/extrachill".to_string()],
            artifact_policy: Some("deny".to_string()),
            ..Default::default()
        };

        let allowed = RunnerExecOptions {
            cwd: Some("/srv/homeboy/extrachill/homeboy".to_string()),
            project_id: Some("extrachill".to_string()),
            allow_diagnostic_ssh: true,
            command: vec!["cargo".to_string(), "test".to_string()],
            env: Default::default(),
            secret_env_names: Vec::new(),
            capture_patch: false,
            raw_exec: true,
            source_snapshot: None,
            capability_preflight: None,
            required_extensions: Vec::new(),
            require_paths: Vec::new(),
        };
        validate_runner_policy(
            &runner,
            "/srv/homeboy/extrachill/homeboy",
            policy_request(&allowed),
        )
        .expect("allowed policy");

        let mut denied_project = allowed.clone();
        denied_project.project_id = Some("wire".to_string());
        assert_eq!(
            validate_runner_policy(
                &runner,
                "/srv/homeboy/extrachill/homeboy",
                policy_request(&denied_project),
            )
            .expect_err("deny project")
            .code
            .as_str(),
            "runner.policy_denied"
        );

        let mut denied_command = allowed.clone();
        denied_command.command = vec!["sh".to_string()];
        assert!(validate_runner_policy(
            &runner,
            "/srv/homeboy/extrachill/homeboy",
            policy_request(&denied_command)
        )
        .expect_err("deny command")
        .message
        .contains("command family 'sh'"));

        assert!(
            validate_runner_policy(&runner, "/srv/homeboy/other", policy_request(&allowed))
                .expect_err("deny workspace")
                .message
                .contains("workspace roots")
        );

        let mut denied_artifacts = allowed.clone();
        denied_artifacts.capture_patch = true;
        assert!(validate_runner_policy(
            &runner,
            "/srv/homeboy/extrachill/homeboy",
            policy_request(&denied_artifacts)
        )
        .expect_err("deny artifacts")
        .message
        .contains("artifact capture"));
    }

    #[test]
    fn test_daemon_api_get_requires_connected_runner() {
        crate::test_support::with_isolated_home(|_| {
            super::super::create(
                r#"{"id":"lab-local","kind":"local","workspace_root":"/tmp"}"#,
                false,
            )
            .expect("create local runner");

            let err = daemon_api_get("lab-local", "/runs").expect_err("requires daemon");
            assert_eq!(err.code.as_str(), "validation.invalid_argument");
            assert!(err.message.contains("connected to a daemon"));
        });
    }

    #[test]
    fn canonical_daemon_body_requires_nested_body() {
        let err = canonical_daemon_body(&json!({ "job": {} }), "daemon exec response")
            .expect_err("reject legacy direct data");
        assert!(err.message.contains("data.body"));
    }

    #[test]
    fn canonical_daemon_body_returns_nested_body() {
        let data = json!({ "body": { "job": { "id": "job-1" } } });
        let body = canonical_daemon_body(&data, "daemon exec response").expect("body");
        assert_eq!(body["job"]["id"], "job-1");
    }

    #[test]
    fn runner_exec_wait_timeout_defaults_to_controller_timeout_budget() {
        std::env::remove_var(RUNNER_EXEC_WAIT_TIMEOUT_ENV);
        assert_eq!(runner_exec_wait_timeout(), Duration::from_secs(20 * 60));
    }

    #[test]
    fn timeout_mirrors_remote_job_without_cancelling() {
        crate::test_support::with_isolated_home(|_| {
            let runner = ssh_runner();
            let job_id = uuid::Uuid::new_v4();
            let job = Job {
                id: job_id,
                operation: "runner.exec".to_string(),
                status: JobStatus::Running,
                created_at_ms: 1_700_000_000_000,
                updated_at_ms: 1_700_000_001_000,
                started_at_ms: Some(1_700_000_000_000),
                finished_at_ms: None,
                event_count: 0,
                source_snapshot: None,
                stale_reason: None,
                target_runner_id: None,
                target_project_id: None,
                claim_id: None,
                claimed_by_runner_id: None,
                claimed_at_ms: None,
                claim_expires_at_ms: None,
                artifacts: Vec::new(),
            };
            let err = daemon_job_wait_timeout(
                &runner,
                "/srv/homeboy/project",
                &["homeboy".to_string(), "bench".to_string()],
                &job,
                &[],
                "runner daemon job",
            );
            let run_id = format!("runner-exec-lab-{job_id}");

            assert!(err.message.contains("runner daemon job"));
            assert!(err.message.contains(job_id.to_string().as_str()));
            assert!(err.message.contains("lab"));
            assert!(err.message.contains("was not cancelled"));
            assert!(err.hints.iter().any(|hint| hint
                .message
                .contains(&format!("homeboy runs show {run_id}"))));
            assert!(err.hints.iter().any(|hint| hint
                .message
                .contains(&format!("homeboy runs artifacts {run_id}"))));
            assert!(err.hints.iter().any(|hint| hint
                .message
                .contains("Lab offload handoff: runner `lab` has daemon job")));
            assert!(err.hints.iter().any(|hint| hint.message.contains(
                "homeboy runner exec lab --cwd /srv/homeboy/project -- homeboy runs list --status running --limit 20"
            )));
            assert!(err.hints.iter().any(|hint| hint
                .message
                .contains(&format!("homeboy runner job cancel lab {job_id}"))));
            assert!(err.hints.iter().any(|hint| {
                hint.message.contains(RUNNER_EXEC_WAIT_TIMEOUT_ENV)
                    && hint.message.contains("controller-side")
                    && hint.message.contains("workload settings")
            }));

            let store =
                crate::core::observation::ObservationStore::open_initialized().expect("store");
            let mirrored = store
                .get_run(&run_id)
                .expect("get mirrored run")
                .expect("mirrored run");
            assert_eq!(mirrored.status, "running");
            assert_eq!(
                mirrored.metadata_json["lab"]["remote_job"]["id"].as_str(),
                Some(job_id.to_string().as_str())
            );
        });
    }

    #[test]
    fn lab_offload_handoff_hints_render_durable_commands() {
        let hints = lab_offload_handoff_hints(
            "homeboy-lab",
            Some("/home/user/Developer/project with spaces"),
            "job-123",
            Some("run-456"),
            DaemonJobHandoffState::InFlight,
        );
        let joined = hints.join("\n");

        assert!(joined.contains("runner `homeboy-lab`"));
        assert!(joined.contains("daemon job `job-123`"));
        assert!(joined.contains("still in flight"));
        assert!(joined.contains("Persisted run id: `run-456`"));
        assert!(joined.contains("homeboy runs show run-456"));
        assert!(joined.contains("homeboy runs evidence run-456"));
        assert!(joined.contains("homeboy runs artifacts run-456"));
        assert!(joined.contains(
            "homeboy runner exec homeboy-lab --cwd '/home/user/Developer/project with spaces' -- homeboy runs list --status running --limit 20"
        ));
        assert!(joined.contains("homeboy runner job logs homeboy-lab job-123 --follow"));
        assert!(joined.contains("homeboy runner job cancel homeboy-lab job-123"));
    }

    #[test]
    fn terminal_handoff_hints_reflect_succeeded_job_state() {
        let hints = lab_offload_handoff_hints(
            "homeboy-lab",
            Some("/srv/homeboy/project"),
            "job-123",
            Some("run-456"),
            DaemonJobHandoffState::Terminal(JobStatus::Succeeded),
        );
        let joined = hints.join("\n");

        assert!(joined.contains("finished with status `succeeded`"));
        assert!(joined.contains("homeboy runs show run-456"));
        assert!(joined.contains("homeboy runs evidence run-456"));
        assert!(joined.contains("homeboy runs artifacts run-456"));
        assert!(joined.contains("Final daemon job events/result"));
        assert!(joined.contains("homeboy runner job logs homeboy-lab job-123"));
        assert!(!joined.contains("still in flight"));
        assert!(!joined.contains("homeboy runner job cancel homeboy-lab job-123"));
    }

    #[test]
    fn terminal_handoff_hints_reflect_failed_job_state() {
        let hints = lab_offload_handoff_hints(
            "homeboy-lab",
            Some("/srv/homeboy/project"),
            "job-123",
            Some("run-456"),
            DaemonJobHandoffState::Terminal(JobStatus::Failed),
        );
        let joined = hints.join("\n");

        assert!(joined.contains("finished with status `failed`"));
        assert!(joined.contains("Final daemon job events/result"));
        assert!(!joined.contains("still in flight"));
    }

    #[test]
    fn terminal_handoff_hints_reflect_cancelled_job_state() {
        let hints = lab_offload_handoff_hints(
            "homeboy-lab",
            Some("/srv/homeboy/project"),
            "job-123",
            None,
            DaemonJobHandoffState::Terminal(JobStatus::Cancelled),
        );
        let joined = hints.join("\n");

        assert!(joined.contains("finished with status `cancelled`"));
        assert!(joined.contains("Persisted runner-side run id is not known"));
        assert!(joined.contains("Final daemon job events/result"));
        assert!(!joined.contains("still in flight"));
        assert!(!joined.contains("--status running"));
    }

    #[test]
    fn lab_offload_handoff_persists_run_when_job_is_accepted() {
        crate::test_support::with_isolated_home(|_| {
            let runner = ssh_runner();
            let job_id = uuid::Uuid::new_v4();
            let job = Job {
                id: job_id,
                operation: "runner.exec".to_string(),
                status: JobStatus::Running,
                created_at_ms: 1_700_000_000_000,
                updated_at_ms: 1_700_000_001_000,
                started_at_ms: Some(1_700_000_000_000),
                finished_at_ms: None,
                event_count: 0,
                source_snapshot: None,
                stale_reason: None,
                target_runner_id: None,
                target_project_id: None,
                claim_id: None,
                claimed_by_runner_id: None,
                claimed_at_ms: None,
                claim_expires_at_ms: None,
                artifacts: Vec::new(),
            };

            let run_id = persist_lab_offload_handoff_run(
                &runner,
                "/srv/homeboy/project",
                &["homeboy".to_string(), "trace".to_string()],
                &job,
            )
            .expect("persist handoff run");

            assert_eq!(run_id, format!("runner-exec-lab-{job_id}"));
            let store =
                crate::core::observation::ObservationStore::open_initialized().expect("store");
            let run = store
                .get_run(&run_id)
                .expect("get run")
                .expect("persisted handoff run");
            assert_eq!(run.status, "running");
            assert_eq!(run.cwd.as_deref(), Some("/srv/homeboy/project"));
            assert_eq!(
                run.metadata_json["lab"]["remote_job"]["id"].as_str(),
                Some(job_id.to_string().as_str())
            );
        });
    }

    #[test]
    fn reverse_broker_exec_submits_job_and_polls_result() {
        crate::test_support::with_isolated_home(|_| {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("listener");
            let addr = listener.local_addr().expect("addr");
            std::thread::spawn(move || {
                let _ = crate::core::daemon::serve_listener(listener);
            });
            let broker_url = format!("http://{addr}");
            let worker_broker_url = broker_url.clone();
            let worker = std::thread::spawn(move || {
                let client = Client::builder()
                    .timeout(Duration::from_secs(10))
                    .build()
                    .expect("client");
                let claim = loop {
                    let response: Value = client
                        .post(format!("{}/runner/jobs/claim", worker_broker_url))
                        .json(&json!({
                            "runner_id": "lab",
                            "lease_ms": 30_000,
                        }))
                        .send()
                        .expect("claim response")
                        .json()
                        .expect("claim json");
                    let claim = response["data"]["body"]["claim"].clone();
                    if !claim.is_null() {
                        break claim;
                    }
                    std::thread::sleep(Duration::from_millis(20));
                };
                let job_id = claim["job"]["id"].as_str().expect("job id").to_string();
                client
                    .post(format!("{}/runner/jobs/{job_id}/events", worker_broker_url))
                    .json(&json!({
                        "runner_id": "lab",
                        "kind": "progress",
                        "message": "running test worker"
                    }))
                    .send()
                    .expect("event response");
                client
                    .post(format!("{}/runner/jobs/{job_id}/finish", worker_broker_url))
                    .json(&json!({
                        "runner_id": "lab",
                        "result": {
                            "exit_code": 0,
                            "stdout": "reverse ok",
                            "stderr": ""
                        }
                    }))
                    .send()
                    .expect("finish response");
            });

            let (output, exit_code) = exec_via_reverse_broker(
                &ssh_runner(),
                &broker_url,
                "/srv/homeboy/project".to_string(),
                Some("extrachill".to_string()),
                vec!["homeboy".to_string(), "test".to_string()],
                Default::default(),
                Vec::new(),
                false,
                None,
                Vec::new(),
            )
            .expect("reverse broker exec");
            worker.join().expect("worker joins");

            assert_eq!(exit_code, 0);
            assert_eq!(output.mode, RunnerExecMode::ReverseBroker);
            assert_eq!(output.stdout, "reverse ok");
            assert_eq!(output.runner_id, "lab");
            assert!(output.job_id.is_some());
            assert!(output
                .job_events
                .expect("events")
                .iter()
                .any(|event| { event.kind == crate::core::api_jobs::JobEventKind::Progress }));
        });
    }

    #[test]
    fn daemon_exec_failure_without_error_field_is_actionable() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("listener");
        let addr = listener.local_addr().expect("addr");
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut buffer = [0; 4096];
            let _ = std::io::Read::read(&mut stream, &mut buffer).expect("read request");
            let body = serde_json::json!({
                "success": false,
                "data": {
                    "error": "validation.invalid_argument",
                    "message": "Invalid argument 'cwd': runner exec requires an absolute cwd"
                }
            })
            .to_string();
            let response = format!(
                "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            std::io::Write::write_all(&mut stream, response.as_bytes()).expect("write response");
        });

        let err = exec_via_daemon(
            &ssh_runner(),
            &format!("http://{addr}"),
            "/srv/homeboy/project".to_string(),
            None,
            vec!["homeboy".to_string(), "--version".to_string()],
            Default::default(),
            Vec::new(),
            false,
            None,
            Vec::new(),
        )
        .expect_err("daemon exec failure");

        assert!(err.message.contains("daemon exec request failed"));
        assert!(err.message.contains("validation.invalid_argument"));
        assert!(err.message.contains("runner exec requires an absolute cwd"));
        assert!(!err.message.contains(": null"));
    }

    #[test]
    fn daemon_exec_request_failed_error_surfaces_payload_detail() {
        let envelope = DaemonEnvelope {
            success: false,
            data: Some(serde_json::json!({
                "error": "validation.invalid_argument",
                "message": "bad cwd"
            })),
            error: None,
        };
        let err = daemon_exec_request_failed_error("lab", 400, &envelope);
        assert!(err.message.contains("daemon exec request failed"));
        assert!(err.message.contains("validation.invalid_argument"));
        assert!(err.message.contains("bad cwd"));
        assert!(!err.message.contains("null"));
    }

    #[test]
    fn daemon_exec_request_failed_error_handles_null_payload_with_reconnect_hint() {
        // The historical #3631/#3624 symptom: a stale/restarting daemon answers
        // with an empty/null error payload. We must never surface a bare `null`,
        // and we must point the operator at reconnecting.
        let envelope = DaemonEnvelope {
            success: false,
            data: None,
            error: Some(Value::Null),
        };
        let err = daemon_exec_request_failed_error("lab", 502, &envelope);
        assert!(!err.message.contains("null"));
        assert!(err.message.contains("stale") || err.message.contains("restarted"));
        assert!(err
            .hints
            .iter()
            .any(|hint| hint.message.contains("homeboy runner connect lab")));
    }

    #[test]
    fn daemon_exec_stale_response_error_is_actionable() {
        let err = daemon_exec_stale_response_error("lab", 200, "expected value at line 1 column 1");
        assert!(err.message.contains("unreadable exec response"));
        assert!(err
            .hints
            .iter()
            .any(|hint| hint.message.contains("homeboy runner connect lab")));
    }

    #[test]
    fn daemon_exec_empty_envelope_over_http_is_actionable_not_null() {
        // A stale daemon that answers `{"success": false}` with no error/data.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("listener");
        let addr = listener.local_addr().expect("addr");
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut buffer = [0; 4096];
            let _ = std::io::Read::read(&mut stream, &mut buffer).expect("read request");
            let body = serde_json::json!({ "success": false }).to_string();
            let response = format!(
                "HTTP/1.1 502 Bad Gateway\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            std::io::Write::write_all(&mut stream, response.as_bytes()).expect("write response");
        });

        let err = exec_via_daemon(
            &ssh_runner(),
            &format!("http://{addr}"),
            "/srv/homeboy/project".to_string(),
            None,
            vec!["homeboy".to_string(), "--version".to_string()],
            Default::default(),
            Vec::new(),
            false,
            None,
            Vec::new(),
        )
        .expect_err("daemon exec failure");

        assert!(!err.message.contains(": null"));
        assert!(err.message.contains("no result") || err.message.contains("stale"));
        assert!(err
            .hints
            .iter()
            .any(|hint| hint.message.contains("homeboy runner connect")));
    }
}
