use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant};

use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::core::api_jobs::{Job, JobEvent, JobStatus, RemoteRunnerJobRequest};
use crate::core::engine::command::CommandCaptureMetadata;
use crate::core::engine::shell;
use crate::core::error::{Error, ErrorCode, Result};
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
use super::{load, status, Runner, RunnerCapabilityPreflight, RunnerKind, RunnerTunnelMode};
use super::{normalize_runner_command_env, resolve_runner_secret_env};

const DEFAULT_RUNNER_EXEC_WAIT_TIMEOUT_SECS: u64 = 20 * 60;
pub(crate) const RUNNER_EXEC_WAIT_TIMEOUT_ENV: &str = "HOMEBOY_RUNNER_EXEC_WAIT_TIMEOUT_SECS";

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

    let plan = prepare_runner_process(RunnerProcessRequest {
        runner_id: runner_id.to_string(),
        runner: None,
        cwd: options.cwd.clone(),
        project_id: options.project_id.clone(),
        command: options.command.clone(),
        env: options.env.clone(),
        capture_patch: options.capture_patch,
        raw_exec: options.raw_exec,
        source_snapshot: options.source_snapshot.clone(),
        require_paths: options.require_paths.clone(),
        validate_require_paths_on_host: false,
    })?;
    let runner = plan.runner.clone();
    let cwd = plan.cwd.clone();
    let connected = status(runner_id)?;
    let request_env = plan.env.clone();
    let required_extensions =
        required_extensions_for_command(&options.command, &options.required_extensions);

    validate_runner_extension_parity(runner_id, &runner, &cwd, &required_extensions)?;

    if connected.connected {
        if let Some(session) = connected.session {
            preflight_runner_capability_plan(
                &runner,
                options.capability_preflight.as_ref(),
                &request_env,
            )?;
            if let Some(local_url) = session.local_url.as_deref() {
                return exec_via_daemon(
                    &runner,
                    local_url,
                    cwd,
                    options.project_id,
                    options.command,
                    request_env,
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
        RunnerKind::Ssh if options.allow_diagnostic_ssh => {
            preflight_runner_capability_plan(
                &runner,
                options.capability_preflight.as_ref(),
                &request_env,
            )?;
            exec_diagnostic_ssh(&runner, cwd, options.command, request_env, options.require_paths)
        }
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

fn exec_via_reverse_broker(
    runner: &Runner,
    broker_url: &str,
    cwd: String,
    project_id: Option<String>,
    command: Vec<String>,
    env: HashMap<String, String>,
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
    let request = RemoteRunnerJobRequest {
        runner_id: runner.id.clone(),
        project_id,
        operation: "runner.exec".to_string(),
        command: command.clone(),
        cwd: Some(cwd.clone()),
        env,
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

    let deadline = Instant::now() + runner_exec_wait_timeout();
    while !matches!(
        job.status,
        JobStatus::Succeeded | JobStatus::Failed | JobStatus::Cancelled
    ) {
        if Instant::now() >= deadline {
            let events =
                fetch_daemon_events(&client, broker_url, &job.id.to_string()).unwrap_or_default();
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
        job = fetch_daemon_job(&client, broker_url, &job.id.to_string())?;
    }
    let events = fetch_daemon_events(&client, broker_url, &job.id.to_string())?;

    let result = result_event_data(&events).unwrap_or_else(|| json!({}));
    let stdout = string_field(&result, "stdout");
    let stderr = string_field(&result, "stderr");
    let metrics = result
        .get("metrics")
        .and_then(|value| serde_json::from_value(value.clone()).ok());
    let exit_code = result
        .get("exit_code")
        .and_then(Value::as_i64)
        .and_then(|code| i32::try_from(code).ok())
        .unwrap_or_else(|| {
            if job.status == JobStatus::Succeeded {
                0
            } else {
                1
            }
        });

    Ok((
        RunnerExecOutput {
            command: "runner.exec",
            runner_id: runner.id.clone(),
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

fn exec_via_daemon(
    runner: &Runner,
    local_url: &str,
    cwd: String,
    project_id: Option<String>,
    command: Vec<String>,
    env: HashMap<String, String>,
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
            "capture_patch": capture_patch,
            "source_snapshot": source_snapshot.clone(),
            "require_paths": require_paths.clone(),
        }))
        .send()
        .map_err(|err| {
            Error::internal_unexpected(format!("submit runner daemon exec job: {err}"))
        })?;
    let status_code = response.status().as_u16();
    let envelope: DaemonEnvelope = response.json().map_err(|err| {
        Error::internal_json(
            err.to_string(),
            Some("parse daemon exec response".to_string()),
        )
    })?;
    if status_code >= 400 || !envelope.success {
        return Err(Error::internal_unexpected(format!(
            "daemon exec request failed: {}",
            daemon_failure_message(status_code, &envelope)
        )));
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

    let deadline = Instant::now() + runner_exec_wait_timeout();
    while !matches!(
        job.status,
        JobStatus::Succeeded | JobStatus::Failed | JobStatus::Cancelled
    ) {
        if Instant::now() >= deadline {
            let events =
                fetch_daemon_events(&client, local_url, &job.id.to_string()).unwrap_or_default();
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
        job = fetch_daemon_job(&client, local_url, &job_id)
            .map_err(|err| daemon_job_context_error(&runner.id, &job_id, err))?;
    }
    let job_id = job.id.to_string();
    let events = fetch_daemon_events(&client, local_url, &job_id)
        .map_err(|err| daemon_job_context_error(&runner.id, &job_id, err))?;

    let result = result_event_data(&events).unwrap_or_else(|| json!({}));
    let stdout = string_field(&result, "stdout");
    let stderr = string_field(&result, "stderr");
    let metrics = result
        .get("metrics")
        .and_then(|value| serde_json::from_value(value.clone()).ok());
    let exit_code = result
        .get("exit_code")
        .and_then(Value::as_i64)
        .and_then(|code| i32::try_from(code).ok())
        .unwrap_or_else(|| {
            if job.status == JobStatus::Succeeded {
                0
            } else {
                1
            }
        });

    let mirror = mirror_daemon_evidence(runner, &cwd, &command, &job, &events, &result)?;
    let patch = mirror.as_ref().and_then(|evidence| evidence.patch.clone());

    Ok((
        RunnerExecOutput {
            command: "runner.exec",
            runner_id: runner.id.clone(),
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
    let timeout_hint = format!(
        "Set controller-side `{RUNNER_EXEC_WAIT_TIMEOUT_ENV}` before invoking homeboy to change this wait budget, e.g. `{RUNNER_EXEC_WAIT_TIMEOUT_ENV}=2400 homeboy ...`; workload settings are applied inside the remote job and cannot extend the controller wait."
    );
    let mut error = Error::internal_unexpected(format!(
        "{label} {job_id} on runner {} did not finish before timeout; the remote job is still in flight and was not cancelled",
        runner.id
    ));
    match mirrored {
        Ok(run) => {
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
    error
        .with_hint(format!(
            "Check remote progress with `homeboy runs list --runner {} --status running --limit 20`.",
            runner.id
        ))
        .with_hint(format!(
            "Tail the dispatched runner daemon job with `homeboy runner job logs {} {job_id} --follow`.",
            runner.id
        ))
        .with_hint(format!(
            "Runner daemon job id `{job_id}` was already dispatched; wait for it to finish or cancel it explicitly through the runner daemon if needed."
        ))
        .with_hint(timeout_hint)
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
    daemon_get(&client, local_url, path)
}

fn result_event_data(events: &[JobEvent]) -> Option<Value> {
    events
        .iter()
        .rev()
        .find(|event| matches!(event.kind, crate::core::api_jobs::JobEventKind::Result))
        .and_then(|event| event.data.clone())
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
    if runner.kind == RunnerKind::Local {
        env.extend(resolve_runner_secret_env(&runner.secret_env)?);
    }
    normalize_runner_command_env(&mut env);

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
    env.extend(resolve_runner_secret_env(&runner.secret_env)?);
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
) -> (RunnerExecOutput, i32) {
    let exit_code = output.exit_code;
    (
        RunnerExecOutput {
            command: "runner.exec",
            runner_id: runner.id.clone(),
            mode,
            argv: command,
            remote_cwd: cwd,
            exit_code,
            stdout: output.stdout,
            stderr: output.stderr,
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

fn daemon_failure_message(status_code: u16, envelope: &DaemonEnvelope) -> String {
    let payload = envelope
        .error
        .as_ref()
        .or(envelope.data.as_ref())
        .filter(|value| !value.is_null());
    let Some(payload) = payload else {
        return format!("HTTP {status_code} without error payload");
    };

    let code = payload.get("error").and_then(Value::as_str);
    let message = payload.get("message").and_then(Value::as_str);
    match (code, message) {
        (Some(code), Some(message)) => format!("{code}: {message}"),
        (Some(code), None) => code.to_string(),
        (None, Some(message)) => message.to_string(),
        (None, None) => payload.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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

    fn failed_runner_exec_output(stdout: &str, stderr: &str) -> RunnerExecOutput {
        RunnerExecOutput {
            command: "runner.exec",
            runner_id: "lab".to_string(),
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
    fn runner_exec_failure_error_promotes_homeboy_stdout_error() {
        let output = failed_runner_exec_output(
            r#"{"success":false,"error":{"code":"validation.invalid_argument","message":"Invalid argument 'source': Path does not exist: /Users/chubes/Developer/homeboy-extensions/wordpress","details":{"field":"source"}}}"#,
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
    fn remote_daemon_secret_env_refs_resolve_only_on_runner_side() {
        crate::test_support::with_isolated_home(|_| {
            let temp = tempfile::tempdir().expect("tempdir");
            let workspace = temp.path().join("workspace");
            std::fs::create_dir_all(&workspace).expect("workspace");
            let secret_file = temp.path().join("runner-secret");
            std::fs::write(&secret_file, "dummy-runner-secret\n").expect("secret file");
            let missing_controller_file = temp.path().join("missing-controller-secret");

            let mut controller_runner = ssh_runner();
            controller_runner.workspace_root = Some(workspace.display().to_string());
            controller_runner.secret_env.insert(
                "OPENAI_API_KEY".to_string(),
                RunnerSecretEnvRef {
                    env: None,
                    file: Some(missing_controller_file.display().to_string()),
                },
            );

            let controller_plan = prepare_runner_process(RunnerProcessRequest {
                runner_id: "lab".to_string(),
                runner: Some(controller_runner),
                cwd: Some(workspace.display().to_string()),
                project_id: None,
                command: vec!["true".to_string()],
                env: Default::default(),
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
            .expect("controller prep keeps secret refs unresolved for SSH runner");

            assert!(!controller_plan.env.contains_key("OPENAI_API_KEY"));

            let mut daemon_runner = ssh_runner();
            daemon_runner.workspace_root = Some(workspace.display().to_string());
            daemon_runner.secret_env.insert(
                "OPENAI_API_KEY".to_string(),
                RunnerSecretEnvRef {
                    env: None,
                    file: Some(secret_file.display().to_string()),
                },
            );

            let daemon_plan = prepare_daemon_local_process(RunnerProcessRequest {
                runner_id: "lab".to_string(),
                runner: Some(daemon_runner),
                cwd: Some(workspace.display().to_string()),
                project_id: None,
                command: vec!["true".to_string()],
                env: Default::default(),
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
                daemon_plan.env.get("OPENAI_API_KEY").map(String::as_str),
                Some("dummy-runner-secret")
            );
        });
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
                r#"{"id":"lab-server","host":"192.168.86.63","user":"chubes"}"#,
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
}
