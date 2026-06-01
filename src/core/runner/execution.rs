use std::collections::HashMap;
use std::time::{Duration, Instant};

use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::core::api_jobs::{Job, JobEvent, JobStatus, RemoteRunnerJobRequest};
use crate::core::engine::command::CommandCaptureMetadata;
use crate::core::engine::shell;
use crate::core::error::{Error, Result};
use crate::core::server::{self, SshClient};
use crate::core::source_snapshot::SourceSnapshot;

use super::broker_http;
use super::capabilities::{runner_capability_snapshot, validate_runner_capability_preflight};
use super::evidence::mirror_daemon_evidence;
use super::resource_metrics::{measured_command_output, RunnerResourceMetrics};
use super::{load, status, Runner, RunnerCapabilityPreflight, RunnerKind, RunnerTunnelMode};

mod policy;
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

    let runner = load(runner_id)?;
    let cwd = resolve_cwd(&runner, options.cwd.as_deref())?;
    validate_runner_policy(
        &runner,
        &cwd,
        RunnerPolicyRequest {
            project_id: options.project_id.as_deref(),
            command: &options.command,
            capture_patch: options.capture_patch,
            raw_exec: options.raw_exec,
        },
    )?;
    let connected = status(runner_id)?;
    let mut request_env = runner.env.clone();
    request_env.extend(options.env.clone());
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
                    options.command,
                    request_env,
                    options.capture_patch,
                    options.source_snapshot,
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
                        options.source_snapshot,
                    );
                }
            }
        }
    }

    match runner.kind {
        RunnerKind::Local => exec_local(&runner, cwd, options.command, options.env),
        RunnerKind::Ssh if options.allow_diagnostic_ssh => {
            preflight_runner_capability_plan(
                &runner,
                options.capability_preflight.as_ref(),
                &request_env,
            )?;
            exec_diagnostic_ssh(&runner, cwd, options.command, request_env)
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

fn required_extensions_for_command(command: &[String], explicit: &[String]) -> Vec<String> {
    let mut extensions = explicit
        .iter()
        .filter(|extension| !extension.trim().is_empty())
        .cloned()
        .collect::<Vec<_>>();

    let mut args = command.iter();
    while let Some(arg) = args.next() {
        if arg == "--extension" {
            if let Some(extension) = args.next().filter(|value| !value.trim().is_empty()) {
                push_unique(&mut extensions, extension.to_string());
            }
            continue;
        }
        if let Some(extension) = arg.strip_prefix("--extension=") {
            if !extension.trim().is_empty() {
                push_unique(&mut extensions, extension.to_string());
            }
        }
    }

    extensions
}

fn push_unique(items: &mut Vec<String>, item: String) {
    if !items.contains(&item) {
        items.push(item);
    }
}

pub(crate) fn validate_runner_extension_parity(
    runner_id: &str,
    runner: &Runner,
    cwd: &str,
    required_extensions: &[String],
) -> Result<()> {
    for extension_id in required_extensions {
        validate_runner_extension(runner_id, runner, cwd, extension_id)?;
    }

    Ok(())
}

fn validate_runner_extension(
    runner_id: &str,
    runner: &Runner,
    cwd: &str,
    extension_id: &str,
) -> Result<()> {
    let homeboy_path = runner.settings.homeboy_path.as_deref().unwrap_or("homeboy");
    let command = format!(
        "cd {} && {} extension show {}",
        shell::quote_path(cwd),
        shell::quote_path(homeboy_path),
        shell::quote_arg(extension_id)
    );
    let output = match runner.kind {
        RunnerKind::Local => server::execute_local_command(&command),
        RunnerKind::Ssh => {
            let client = ssh_client_for_runner_extension_parity(runner)?;
            client.execute(&command)
        }
    };

    if output.success {
        return Ok(());
    }

    Err(Error::validation_invalid_argument(
        "runner_extension",
        format!(
            "Runner '{runner_id}' is missing required extension parity for '{extension_id}' before command execution"
        ),
        Some(extension_id.to_string()),
        Some(vec![
            format!(
                "Install the extension on the runner before dispatch: {homeboy_path} extension install <source> --id {extension_id}"
            ),
            format!("Remote preflight command failed: {homeboy_path} extension show {extension_id}"),
            extension_parity_diagnostic_tail(&output.stderr, &output.stdout),
        ]),
    ))
}

fn ssh_client_for_runner_extension_parity(runner: &Runner) -> Result<SshClient> {
    let server_id = runner.server_id.as_deref().ok_or_else(|| {
        Error::validation_invalid_argument(
            "server_id",
            "SSH runners require server_id for runner extension parity preflight",
            Some(runner.id.clone()),
            None,
        )
    })?;
    let server = server::load(server_id)?;
    let mut client = SshClient::from_server(&server, server_id)?;
    client.env.extend(runner.env.clone());
    Ok(client)
}

fn extension_parity_diagnostic_tail(stderr: &str, stdout: &str) -> String {
    let output = if stderr.trim().is_empty() {
        stdout
    } else {
        stderr
    };
    let tail = output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .rev()
        .take(3)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n");

    if tail.is_empty() {
        "Runner extension parity preflight produced no diagnostic output.".to_string()
    } else {
        format!("Runner extension parity preflight output:\n{tail}")
    }
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

    let deadline = Instant::now() + Duration::from_secs(60 * 60);
    while !matches!(
        job.status,
        JobStatus::Succeeded | JobStatus::Failed | JobStatus::Cancelled
    ) {
        if Instant::now() >= deadline {
            return Err(Error::internal_unexpected(format!(
                "reverse runner job {} did not finish before timeout",
                job.id
            )));
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
            source_snapshot: Some(source_snapshot),
            job_id: Some(job.id.to_string()),
            job: Some(job),
            job_events: Some(events),
            mirror_run_id: None,
            patch: None,
            metrics,
            capture: None,
        },
        exit_code,
    ))
}

fn exec_via_daemon(
    runner: &Runner,
    local_url: &str,
    cwd: String,
    command: Vec<String>,
    env: HashMap<String, String>,
    capture_patch: bool,
    source_snapshot_override: Option<SourceSnapshot>,
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
            "cwd": cwd,
            "command": command,
            "env": env,
            "capture_patch": capture_patch,
            "source_snapshot": source_snapshot,
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
            envelope.error.unwrap_or(Value::Null)
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

    let deadline = Instant::now() + Duration::from_secs(60 * 60);
    while !matches!(
        job.status,
        JobStatus::Succeeded | JobStatus::Failed | JobStatus::Cancelled
    ) {
        if Instant::now() >= deadline {
            return Err(Error::internal_unexpected(format!(
                "runner daemon job {} did not finish before timeout",
                job.id
            )));
        }
        std::thread::sleep(Duration::from_millis(200));
        job = fetch_daemon_job(&client, local_url, &job.id.to_string())?;
    }
    let events = fetch_daemon_events(&client, local_url, &job.id.to_string())?;

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
            source_snapshot: Some(source_snapshot),
            job_id: Some(job.id.to_string()),
            job: Some(job),
            job_events: Some(events),
            mirror_run_id: mirror.map(|evidence| evidence.run.id),
            patch,
            metrics,
            capture: None,
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

    let capabilities = runner_capability_snapshot(runner)?;
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

fn exec_local(
    runner: &Runner,
    cwd: String,
    command: Vec<String>,
    env: HashMap<String, String>,
) -> Result<(RunnerExecOutput, i32)> {
    let output = command_output(
        std::process::Command::new(&command[0])
            .args(&command[1..])
            .current_dir(&cwd)
            .envs(env),
    )?;
    let source_snapshot = SourceSnapshot::collect_local(
        &runner.id,
        std::path::Path::new(&cwd),
        Some(&cwd),
        "existing_remote",
    );
    Ok(exec_output(
        runner,
        RunnerExecMode::Local,
        cwd,
        command,
        output,
        Some(source_snapshot),
    ))
}

fn exec_diagnostic_ssh(
    runner: &Runner,
    cwd: String,
    command: Vec<String>,
    env: HashMap<String, String>,
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
    ))
}

struct ProcessOutput {
    stdout: String,
    stderr: String,
    exit_code: i32,
    metrics: Option<RunnerResourceMetrics>,
    capture: Option<CommandCaptureMetadata>,
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

fn exec_output(
    runner: &Runner,
    mode: RunnerExecMode,
    cwd: String,
    command: Vec<String>,
    output: ProcessOutput,
    source_snapshot: Option<SourceSnapshot>,
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
            source_snapshot,
            job: None,
            job_id: None,
            job_events: None,
            mirror_run_id: None,
            patch: None,
            metrics: output.metrics,
            capture: output.capture,
        },
        exit_code,
    )
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::server::{self, RunnerPolicy, RunnerSettings};

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
            resources: Default::default(),
            policy: RunnerPolicy::default(),
        }
    }

    fn policy_request(options: &RunnerExecOptions) -> RunnerPolicyRequest<'_> {
        RunnerPolicyRequest {
            project_id: options.project_id.as_deref(),
            command: &options.command,
            capture_patch: options.capture_patch,
            raw_exec: options.raw_exec,
        }
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
            "--extension=nodejs".to_string(),
        ];

        assert_eq!(
            required_extensions_for_command(&command, &["wordpress".to_string()]),
            vec![
                "wordpress".to_string(),
                "rust".to_string(),
                "nodejs".to_string(),
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
    fn reverse_broker_exec_submits_job_and_polls_result() {
        crate::test_support::with_isolated_home(|_| {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("listener");
            let addr = listener.local_addr().expect("addr");
            drop(listener);
            std::thread::spawn(move || {
                let _ = crate::core::daemon::serve(addr);
            });
            for _ in 0..100 {
                if std::net::TcpStream::connect(addr).is_ok() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
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
}
