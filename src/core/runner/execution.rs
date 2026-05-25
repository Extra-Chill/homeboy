use std::time::{Duration, Instant};

use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::core::api_jobs::{Job, JobEvent, JobStatus};
use crate::core::engine::shell;
use crate::core::error::{Error, Result};
use crate::core::server::{self, SshClient};
use crate::core::source_snapshot::SourceSnapshot;

use super::evidence::mirror_daemon_evidence;
use super::{load, status, Runner, RunnerKind};

#[derive(Debug, Clone)]
pub struct RunnerExecOptions {
    pub cwd: Option<String>,
    pub allow_ssh: bool,
    pub command: Vec<String>,
    pub capture_patch: bool,
    pub source_snapshot: Option<SourceSnapshot>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunnerExecMode {
    Daemon,
    Local,
    Ssh,
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
    let connected = status(runner_id)?;

    if connected.connected {
        if let Some(session) = connected.session {
            return exec_via_daemon(
                &runner,
                &session.local_url,
                cwd,
                options.command,
                options.capture_patch,
                options.source_snapshot,
            );
        }
    }

    match runner.kind {
        RunnerKind::Local => exec_local(&runner, cwd, options.command),
        RunnerKind::Ssh if options.allow_ssh => exec_ssh(&runner, cwd, options.command),
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

fn exec_via_daemon(
    runner: &Runner,
    local_url: &str,
    cwd: String,
    command: Vec<String>,
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
            "env": runner.env,
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
    let body = data.get("body").unwrap_or(&data);
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
        },
        exit_code,
    ))
}

fn fetch_daemon_job(client: &Client, local_url: &str, job_id: &str) -> Result<Job> {
    let data = daemon_get(client, local_url, &format!("/jobs/{job_id}"))?;
    serde_json::from_value(data["body"]["job"].clone())
        .map_err(|err| Error::internal_json(err.to_string(), Some("parse daemon job".to_string())))
}

fn fetch_daemon_events(client: &Client, local_url: &str, job_id: &str) -> Result<Vec<JobEvent>> {
    let data = daemon_get(client, local_url, &format!("/jobs/{job_id}/events"))?;
    serde_json::from_value(data["body"]["events"].clone()).map_err(|err| {
        Error::internal_json(err.to_string(), Some("parse daemon job events".to_string()))
    })
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
    daemon_get(&client, &session.local_url, path)
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
) -> Result<(RunnerExecOutput, i32)> {
    let output = command_output(
        std::process::Command::new(&command[0])
            .args(&command[1..])
            .current_dir(&cwd),
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

fn exec_ssh(runner: &Runner, cwd: String, command: Vec<String>) -> Result<(RunnerExecOutput, i32)> {
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
        RunnerExecMode::Ssh,
        cwd,
        command,
        ProcessOutput {
            stdout: output.stdout,
            stderr: output.stderr,
            exit_code: output.exit_code,
        },
        Some(source_snapshot),
    ))
}

struct ProcessOutput {
    stdout: String,
    stderr: String,
    exit_code: i32,
}

fn command_output(command: &mut std::process::Command) -> Result<ProcessOutput> {
    let output = command.output().map_err(|err| {
        Error::internal_io(err.to_string(), Some("execute runner command".to_string()))
    })?;
    Ok(ProcessOutput {
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        exit_code: output.status.code().unwrap_or(1),
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
    use crate::core::server::RunnerSettings;

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
                    allow_ssh: false,
                    command: vec!["sh".to_string(), "-c".to_string(), "printf ok".to_string()],
                    capture_patch: false,
                    source_snapshot: None,
                },
            )
            .expect("exec local runner");

            assert_eq!(exit_code, 0);
            assert_eq!(output.runner_id, "lab-local");
            assert_eq!(output.mode, RunnerExecMode::Local);
            assert_eq!(output.stdout, "ok");
            let source_snapshot = output.source_snapshot.expect("source snapshot");
            assert_eq!(source_snapshot.runner_id, "lab-local");
            assert_eq!(source_snapshot.sync_mode, "existing_remote");
            assert!(source_snapshot.snapshot_hash.starts_with("sha256:"));
            assert!(output.job_id.is_none());
        });
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
}
