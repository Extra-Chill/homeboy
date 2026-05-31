use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use crate::core::api_jobs::JobStore;
use crate::core::error::{Error, RemoteCommandFailedDetails, Result, TargetDetails};
use crate::core::http_api::{self, AnalysisJobRunner, HttpMethod, UnsupportedAnalysisJobRunner};
use crate::core::paths;
use crate::core::process::pid_is_running;
use crate::core::runner::measured_command_output;
use crate::core::source_snapshot::SourceSnapshot;

mod artifact_download;
mod broker_config;
mod patch_capture;
mod remote_runner;
pub use artifact_download::ArtifactDownload;
pub use broker_config::{render_broker_config, BrokerConfig, BrokerConfigOptions};
use patch_capture::{capture_baseline, capture_patch_report};

pub const DEFAULT_ADDR: &str = "127.0.0.1:0";

static DAEMON_JOB_STORE: OnceLock<JobStore> = OnceLock::new();

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct DaemonState {
    pub address: String,
    pub pid: u32,
    pub state_path: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DaemonStatus {
    pub running: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<DaemonState>,
    pub state_path: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DaemonStartResult {
    pub pid: u32,
    pub address: String,
    pub state_path: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DaemonStopResult {
    pub stopped: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    pub state_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponse {
    pub status_code: u16,
    pub body: serde_json::Value,
    pub artifact: Option<ArtifactDownload>,
}

#[derive(Debug, Clone, Deserialize)]
struct ExecRequest {
    #[serde(default)]
    runner_id: Option<String>,
    cwd: String,
    command: Vec<String>,
    #[serde(default)]
    env: HashMap<String, String>,
    #[serde(default)]
    capture_patch: bool,
    #[serde(default)]
    source_snapshot: Option<SourceSnapshot>,
}

pub fn parse_bind_addr(addr: &str) -> Result<SocketAddr> {
    let parsed: SocketAddr = addr.parse().map_err(|e| {
        Error::validation_invalid_argument(
            "addr",
            format!("Invalid daemon bind address `{}`: {}", addr, e),
            Some(addr.to_string()),
            Some(vec!["Use a host:port value like 127.0.0.1:0".to_string()]),
        )
    })?;

    if !parsed.ip().is_loopback() {
        return Err(Error::validation_invalid_argument(
            "addr",
            "Daemon MVP only accepts loopback bind addresses",
            Some(addr.to_string()),
            Some(vec!["Use 127.0.0.1:<port> or [::1]:<port>".to_string()]),
        ));
    }

    Ok(parsed)
}

fn state_path() -> Result<PathBuf> {
    paths::daemon_state_file()
}

pub fn read_status() -> Result<DaemonStatus> {
    let path = state_path()?;
    let state_path = path.display().to_string();

    if !path.exists() {
        return Ok(DaemonStatus {
            running: false,
            state: None,
            state_path,
        });
    }

    let content = fs::read_to_string(&path)
        .map_err(|e| Error::internal_io(e.to_string(), Some(format!("read {}", path.display()))))?;
    let state: DaemonState = serde_json::from_str(&content)
        .map_err(|e| Error::config_invalid_json(path.display().to_string(), e))?;

    Ok(DaemonStatus {
        running: pid_is_running(state.pid),
        state: Some(state),
        state_path,
    })
}

pub fn stop() -> Result<DaemonStopResult> {
    let status = read_status()?;
    let Some(state) = status.state else {
        return Ok(DaemonStopResult {
            stopped: false,
            pid: None,
            state_path: status.state_path,
        });
    };

    let stopped = if pid_is_running(state.pid) {
        terminate_pid(state.pid)?;
        true
    } else {
        false
    };

    let path = state_path()?;
    if path.exists() {
        fs::remove_file(&path).map_err(|e| {
            Error::internal_io(e.to_string(), Some(format!("delete {}", path.display())))
        })?;
    }

    Ok(DaemonStopResult {
        stopped,
        pid: Some(state.pid),
        state_path: status.state_path,
    })
}

pub fn serve(addr: SocketAddr) -> Result<DaemonState> {
    serve_with_analysis_runner(addr, UnsupportedAnalysisJobRunner)
}

pub fn serve_with_analysis_runner<R>(addr: SocketAddr, analysis_runner: R) -> Result<DaemonState>
where
    R: AnalysisJobRunner,
{
    let listener = TcpListener::bind(addr)
        .map_err(|e| Error::internal_io(e.to_string(), Some(format!("bind daemon to {}", addr))))?;
    let local_addr = listener.local_addr().map_err(|e| {
        Error::internal_io(e.to_string(), Some("read daemon local address".to_string()))
    })?;
    let state = write_state(local_addr)?;
    let job_store = JobStore::open(paths::daemon_jobs_file()?)?;

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let _ = handle_connection(stream, &job_store, analysis_runner.clone());
            }
            Err(err) => {
                return Err(Error::internal_io(
                    err.to_string(),
                    Some("accept daemon connection".to_string()),
                ));
            }
        }
    }

    Ok(state)
}

pub fn route(method: &str, path: &str) -> HttpResponse {
    route_with_job_store(method, path, daemon_job_store())
}

fn route_with_job_store(method: &str, path: &str, job_store: &JobStore) -> HttpResponse {
    route_with_job_store_and_body(method, path, None, job_store)
}

fn route_with_job_store_and_body(
    method: &str,
    path: &str,
    body: Option<serde_json::Value>,
    job_store: &JobStore,
) -> HttpResponse {
    route_with_job_store_and_body_and_runner(
        method,
        path,
        body,
        job_store,
        UnsupportedAnalysisJobRunner,
    )
}

fn route_with_job_store_and_body_and_runner<R>(
    method: &str,
    path: &str,
    body: Option<serde_json::Value>,
    job_store: &JobStore,
    analysis_runner: R,
) -> HttpResponse
where
    R: AnalysisJobRunner,
{
    match (method, path) {
        ("GET", "/health") => HttpResponse {
            status_code: 200,
            body: json!({
                "status": "ok",
                "version": env!("CARGO_PKG_VERSION"),
            }),
            artifact: None,
        },
        ("GET", "/version") => HttpResponse {
            status_code: 200,
            body: json!({
                "version": env!("CARGO_PKG_VERSION"),
            }),
            artifact: None,
        },
        ("GET", "/config/paths") => match config_paths_body() {
            Ok(body) => HttpResponse {
                status_code: 200,
                body,
                artifact: None,
            },
            Err(err) => error_response(500, err),
        },
        ("POST", "/health") | ("POST", "/version") | ("POST", "/config/paths") => HttpResponse {
            status_code: 405,
            body: json!({ "error": "method_not_allowed" }),
            artifact: None,
        },
        ("POST", "/exec") => match enqueue_exec_job(body, job_store) {
            Ok(body) => HttpResponse {
                status_code: 200,
                body: json!({
                    "status": 200,
                    "endpoint": "jobs.exec",
                    "body": body,
                }),
                artifact: None,
            },
            Err(err) => error_response(400, err),
        },
        ("GET", "/exec") => HttpResponse {
            status_code: 405,
            body: json!({ "error": "method_not_allowed" }),
            artifact: None,
        },
        ("POST", "/runner/sessions")
        | ("POST", "/runner/jobs")
        | ("POST", "/runner/jobs/claim") => remote_runner::route(method, path, body, job_store),
        ("GET", "/runner/sessions") | ("GET", "/runner/jobs") | ("GET", "/runner/jobs/claim") => {
            method_not_allowed()
        }
        ("POST", path) if path.starts_with("/runner/jobs/") => {
            remote_runner::route(method, path, body, job_store)
        }
        _ => route_read_only_api(method, path, body, job_store, analysis_runner),
    }
}

pub(super) fn daemon_endpoint_response(endpoint: &str, body: serde_json::Value) -> HttpResponse {
    HttpResponse {
        status_code: 200,
        body: json!({
            "status": 200,
            "endpoint": endpoint,
            "body": body,
        }),
        artifact: None,
    }
}

fn method_not_allowed() -> HttpResponse {
    HttpResponse {
        status_code: 405,
        body: json!({ "error": "method_not_allowed" }),
        artifact: None,
    }
}

fn enqueue_exec_job(
    body: Option<serde_json::Value>,
    job_store: &JobStore,
) -> Result<serde_json::Value> {
    let request: ExecRequest =
        serde_json::from_value(body.unwrap_or_else(|| json!({}))).map_err(|err| {
            Error::validation_invalid_argument(
                "body",
                format!("invalid exec request body: {err}"),
                None,
                None,
            )
        })?;
    if request.command.is_empty() {
        return Err(Error::validation_invalid_argument(
            "command",
            "exec request requires command array",
            None,
            None,
        ));
    }
    if request.cwd.is_empty() || !Path::new(&request.cwd).is_absolute() {
        return Err(Error::validation_invalid_argument(
            "cwd",
            "exec request requires an absolute cwd",
            Some(request.cwd),
            None,
        ));
    }

    let runner_id = request.runner_id.as_deref().unwrap_or("unknown");
    let source_snapshot = request.source_snapshot.clone().or_else(|| {
        Some(SourceSnapshot::existing_remote(
            runner_id,
            &request.cwd,
            None,
        ))
    });

    let summary = json!({
        "runner_id": request.runner_id,
        "cwd": request.cwd,
        "command": request.command,
        "capture_patch": request.capture_patch,
        "source_snapshot": source_snapshot,
    });
    let operation = "runner.exec".to_string();
    let runner = job_store.run_background_with_source_snapshot(
        operation,
        source_snapshot.clone(),
        move |job| {
            job.progress(json!({
                "phase": "started",
                "runner_id": request.runner_id,
                "cwd": request.cwd,
                "command": request.command,
                "capture_patch": request.capture_patch,
                "job_id": job.job_id(),
                "source_snapshot": source_snapshot,
            }))?;
            let baseline = if request.capture_patch {
                Some(capture_baseline(&request.cwd)?)
            } else {
                None
            };
            let mut command = Command::new(&request.command[0]);
            command
                .args(&request.command[1..])
                .envs(request.env.iter())
                .current_dir(&request.cwd);
            if let Some(snapshot) = &source_snapshot {
                command.env(
                    crate::core::observation::SOURCE_SNAPSHOT_METADATA_ENV,
                    serde_json::to_string(snapshot).unwrap_or_default(),
                );
            }
            let measured = measured_command_output(&mut command)?;
            let output = measured.output;
            let metrics = measured.metrics;
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            let exit_code = output.status.code().unwrap_or(1);
            if !stdout.is_empty() {
                job.stdout(stdout.clone())?;
            }
            if !stderr.is_empty() {
                job.stderr(stderr.clone())?;
            }
            job.progress(json!({
                "phase": "finished",
                "exit_code": exit_code,
                "metrics": metrics.clone(),
            }))?;
            let patch = if let Some(baseline) = baseline {
                Some(capture_patch_report(
                    job.job_id(),
                    request.runner_id.as_deref().unwrap_or("unknown"),
                    &request.cwd,
                    &request.command,
                    source_snapshot.as_ref(),
                    &baseline,
                    exit_code,
                )?)
            } else {
                None
            };
            let result = json!({
                "runner_id": request.runner_id,
                "cwd": request.cwd,
                "command": request.command,
                "exit_code": exit_code,
                "stdout": stdout,
                "stderr": stderr,
                "source_snapshot": source_snapshot,
                "patch": patch,
                "metrics": metrics,
            });
            if exit_code != 0 {
                job.result(result.clone())?;
                return Err(Error::remote_command_failed(RemoteCommandFailedDetails {
                    command: request.command.join(" "),
                    exit_code,
                    stdout,
                    stderr,
                    target: TargetDetails {
                        project_id: None,
                        server_id: request.runner_id,
                        host: None,
                    },
                }));
            }

            Ok(result)
        },
    );
    let job = job_store.get(runner.job_id)?;

    Ok(json!({
        "command": "api.runner.exec.enqueue",
        "job": job,
        "poll": {
            "job": format!("/jobs/{}", runner.job_id),
            "events": format!("/jobs/{}/events", runner.job_id),
        },
        "request": summary,
    }))
}

fn daemon_job_store() -> &'static JobStore {
    DAEMON_JOB_STORE.get_or_init(JobStore::default)
}

fn route_read_only_api(
    method: &str,
    path: &str,
    body: Option<serde_json::Value>,
    job_store: &JobStore,
    analysis_runner: impl AnalysisJobRunner,
) -> HttpResponse {
    let method = match method {
        "GET" => HttpMethod::Get,
        "POST" => HttpMethod::Post,
        _ => {
            return HttpResponse {
                status_code: 405,
                body: json!({ "error": "method_not_allowed" }),
                artifact: None,
            };
        }
    };

    if matches!(method, HttpMethod::Get) {
        if let Some(response) = artifact_download::route(path) {
            return response;
        }
    }

    match http_api::handle_with_jobs_and_runner(
        http_api::HttpApiRequest {
            method,
            path: path.to_string(),
            body,
        },
        job_store,
        analysis_runner,
    ) {
        Ok(response) => HttpResponse {
            status_code: response.status,
            body: serde_json::to_value(response)
                .unwrap_or_else(|_| json!({ "error": "internal_json" })),
            artifact: None,
        },
        Err(err) => error_response(404, err),
    }
}

pub(super) fn error_response(status_code: u16, err: Error) -> HttpResponse {
    HttpResponse {
        status_code,
        body: json!({
            "error": err.code.as_str(),
            "message": err.message,
            "details": err.details,
            "hints": err.hints,
        }),
        artifact: None,
    }
}

fn config_paths_body() -> Result<serde_json::Value> {
    Ok(json!({
        "homeboy": paths::homeboy()?.display().to_string(),
        "homeboy_json": paths::homeboy_json()?.display().to_string(),
        "projects": paths::projects()?.display().to_string(),
        "servers": paths::servers()?.display().to_string(),
        "components": paths::components()?.display().to_string(),
        "extensions": paths::extensions()?.display().to_string(),
        "rigs": paths::rigs()?.display().to_string(),
        "stacks": paths::stacks()?.display().to_string(),
        "daemon_state": paths::daemon_state_file()?.display().to_string(),
        "daemon_jobs": paths::daemon_jobs_file()?.display().to_string(),
    }))
}

fn write_state(addr: SocketAddr) -> Result<DaemonState> {
    let path = state_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| {
            Error::internal_io(e.to_string(), Some(format!("create {}", parent.display())))
        })?;
    }

    let state = DaemonState {
        address: addr.to_string(),
        pid: std::process::id(),
        state_path: path.display().to_string(),
    };
    let body = serde_json::to_string_pretty(&state).map_err(|e| {
        Error::internal_json(e.to_string(), Some("serialize daemon state".to_string()))
    })?;
    fs::write(&path, body).map_err(|e| {
        Error::internal_io(e.to_string(), Some(format!("write {}", path.display())))
    })?;
    Ok(state)
}

fn handle_connection<R>(
    mut stream: TcpStream,
    job_store: &JobStore,
    analysis_runner: R,
) -> std::io::Result<()>
where
    R: AnalysisJobRunner,
{
    let mut buffer = [0; 64 * 1024];
    let bytes = stream.read(&mut buffer)?;
    let request = String::from_utf8_lossy(&buffer[..bytes]);
    let mut headers_and_body = request.splitn(2, "\r\n\r\n");
    let headers = headers_and_body.next().unwrap_or_default();
    let body = headers_and_body.next().unwrap_or_default();
    let mut parts = headers
        .lines()
        .next()
        .unwrap_or_default()
        .split_whitespace();
    let method = parts.next().unwrap_or_default();
    let path = parts.next().unwrap_or_default();
    let parsed_body = if body.trim().is_empty() {
        None
    } else {
        match serde_json::from_str::<serde_json::Value>(body.trim()) {
            Ok(value) => Some(value),
            Err(error) => {
                let response = error_response(
                    400,
                    Error::validation_invalid_argument(
                        "body",
                        format!("invalid JSON request body: {error}"),
                        None,
                        None,
                    ),
                );
                return write_http_response(stream, response);
            }
        }
    };
    let response = route_with_job_store_and_body_and_runner(
        method,
        path,
        parsed_body,
        job_store,
        analysis_runner,
    );
    write_http_response(stream, response)
}

fn write_http_response(mut stream: TcpStream, response: HttpResponse) -> std::io::Result<()> {
    if let Some(artifact) = response.artifact {
        return artifact_download::write_response(stream, response.status_code, artifact);
    }

    let body = serde_json::to_string_pretty(&json!({
        "success": (200..300).contains(&response.status_code),
        "data": response.body,
    }))
    .unwrap_or_else(|_| "{\"success\":false}".to_string());
    let status_text = match response.status_code {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        _ => "Internal Server Error",
    };
    write!(
        stream,
        "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        response.status_code,
        status_text,
        body.len(),
        body
    )
}

fn terminate_pid(pid: u32) -> Result<()> {
    #[cfg(unix)]
    unsafe {
        if libc::kill(pid as libc::pid_t, libc::SIGTERM) != 0 {
            return Err(Error::internal_io(
                std::io::Error::last_os_error().to_string(),
                Some(format!("stop daemon pid {}", pid)),
            ));
        }
        Ok(())
    }

    #[cfg(not(unix))]
    {
        let _ = pid;
        Err(Error::internal_unexpected(
            "daemon stop is not implemented on this platform",
        ))
    }
}

#[cfg(test)]
#[path = "../../tests/core/daemon_test.rs"]
mod daemon_test;
