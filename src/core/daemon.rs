use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::UNIX_EPOCH;

use crate::command_contract::RunnerWorkload;
use crate::core::api_jobs::{JobStatus, JobStore};
use crate::core::build_identity;
use crate::core::error::{Error, RemoteCommandFailedDetails, Result, TargetDetails};
use crate::core::http_api::{self, AnalysisJobRunner, HttpMethod, UnsupportedAnalysisJobRunner};
use crate::core::paths;
use crate::core::process::pid_is_running;
use crate::core::runner::{
    execute_runner_process_until_cancelled, prepare_daemon_local_process, Runner,
    RunnerProcessRequest,
};
use crate::core::source_snapshot::SourceSnapshot;
use crate::core::upgrade::VERSION;

mod artifact_download;
mod broker_config;
mod control;
mod patch_capture;
mod remote_runner;
pub use artifact_download::ArtifactDownload;
pub use broker_config::{render_broker_config, BrokerConfig, BrokerConfigOptions, ServiceIdentity};
pub use control::{
    artifact_content_url, fetch_artifact_to_path, start_background, ArtifactFetchOutcome,
};
use patch_capture::{capture_baseline, capture_patch_report};

pub const DEFAULT_ADDR: &str = "127.0.0.1:0";

static DAEMON_JOB_STORE: OnceLock<JobStore> = OnceLock::new();
static DAEMON_RUNTIME_SNAPSHOT: OnceLock<DaemonRuntimeSnapshot> = OnceLock::new();

const RUNTIME_PATH_FILE_LIMIT: usize = 2_000;
const RUNTIME_PATH_SUFFIXES: &[&str] = &[
    "_COMPONENT_PATH",
    "_PLUGIN_PATH",
    "_PROVIDER_PATH",
    "_RUNTIME_PATH",
];

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

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct DaemonRuntimeSnapshot {
    loaded_at: String,
    paths: Vec<DaemonRuntimePathSnapshot>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct DaemonRuntimePathSnapshot {
    env: String,
    path: String,
    fingerprint: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponse {
    pub status_code: u16,
    pub body: serde_json::Value,
    pub artifact: Option<ArtifactDownload>,
}

#[derive(Debug, Clone, Deserialize)]
struct ExecRequest {
    runner_id: String,
    #[serde(default)]
    runner: Option<Runner>,
    #[serde(default)]
    project_id: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
    command: Vec<String>,
    #[serde(default)]
    env: HashMap<String, String>,
    #[serde(default)]
    secret_env_names: Vec<String>,
    #[serde(default)]
    capture_patch: bool,
    #[serde(default)]
    raw_exec: bool,
    #[serde(default)]
    source_snapshot: Option<SourceSnapshot>,
    #[serde(default)]
    require_paths: Vec<String>,
    #[serde(default)]
    runner_workload: Option<RunnerWorkload>,
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
    serve_listener_with_analysis_runner(listener, analysis_runner)
}

#[cfg(test)]
pub(crate) fn serve_listener(listener: TcpListener) -> Result<DaemonState> {
    serve_listener_with_analysis_runner(listener, UnsupportedAnalysisJobRunner)
}

fn serve_listener_with_analysis_runner<R>(
    listener: TcpListener,
    analysis_runner: R,
) -> Result<DaemonState>
where
    R: AnalysisJobRunner,
{
    let local_addr = listener.local_addr().map_err(|e| {
        Error::internal_io(e.to_string(), Some("read daemon local address".to_string()))
    })?;
    let state = write_state(local_addr)?;
    let job_store = JobStore::open(paths::daemon_jobs_file()?)?;
    let _ = daemon_runtime_snapshot();
    let loopback_bind = local_addr.ip().is_loopback();

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let _ =
                    handle_connection(stream, &job_store, analysis_runner.clone(), loopback_bind);
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
    // In-process dispatch (CLI internals, tests) is already inside the trust
    // boundary; only the network entry point (`handle_connection`) authenticates
    // broker traffic against the on-disk auth store.
    route_with_job_store_and_body_and_runner_and_auth(
        method,
        path,
        body,
        job_store,
        analysis_runner,
        remote_runner::BrokerAuthContext::trusted_local(),
    )
}

fn route_with_job_store_and_body_and_runner_and_auth<R>(
    method: &str,
    path: &str,
    body: Option<serde_json::Value>,
    job_store: &JobStore,
    analysis_runner: R,
    broker_auth: remote_runner::BrokerAuthContext,
) -> HttpResponse
where
    R: AnalysisJobRunner,
{
    match (method, path) {
        ("GET", "/health") => HttpResponse {
            status_code: 200,
            body: json!({
                "status": "ok",
                "version": VERSION,
                "build_identity": build_identity::current(),
            }),
            artifact: None,
        },
        ("GET", "/version") => HttpResponse {
            status_code: 200,
            body: json!({
                "version": VERSION,
                "build_identity": build_identity::current(),
                "runtime_paths": daemon_runtime_paths_body(),
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
            Ok(body) => daemon_endpoint_response("jobs.exec", body),
            Err(err) => error_response(400, err),
        },
        ("GET", "/exec") => HttpResponse {
            status_code: 405,
            body: json!({ "error": "method_not_allowed" }),
            artifact: None,
        },
        ("POST", "/runner/sessions")
        | ("POST", "/runner/jobs")
        | ("POST", "/runner/jobs/reconcile")
        | ("POST", "/runner/jobs/claim") => {
            remote_runner::route(method, path, body, job_store, &broker_auth)
        }
        ("GET", "/runner/sessions")
        | ("GET", "/runner/jobs")
        | ("GET", "/runner/jobs/reconcile")
        | ("GET", "/runner/jobs/claim") => method_not_allowed(),
        ("GET", path) if path.starts_with("/runner/jobs/") => {
            remote_runner::route(method, path, body, job_store, &broker_auth)
        }
        ("POST", path) if path.starts_with("/runner/jobs/") => {
            remote_runner::route(method, path, body, job_store, &broker_auth)
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
    let mut request: ExecRequest = serde_json::from_value(body.unwrap_or_else(|| json!({})))
        .map_err(|err| {
            Error::validation_invalid_argument(
                "body",
                format!("invalid exec request body: {err}"),
                None,
                None,
            )
        })?;
    request.secret_env_names = crate::core::runner::runner_exec_secret_env_names(
        &request.command,
        None,
        &request.secret_env_names,
    );
    crate::core::runner::workload::validate_runner_workload_dispatch(
        request.runner_workload.as_ref(),
        &request.runner_id,
        request.cwd.as_deref(),
        &request.command,
        &request.secret_env_names,
        request.capture_patch,
    )?;
    let plan = prepare_daemon_local_process(RunnerProcessRequest {
        runner_id: request.runner_id,
        runner: request.runner,
        cwd: request.cwd,
        project_id: request.project_id,
        command: request.command,
        env: request.env,
        secret_env_names: request.secret_env_names,
        capture_patch: request.capture_patch,
        raw_exec: request.raw_exec,
        source_snapshot: request.source_snapshot,
        require_paths: request.require_paths,
        validate_require_paths_on_host: true,
    })?;
    let source_snapshot = Some(plan.source_snapshot.clone());

    let summary = json!({
        "runner_id": plan.runner.id,
        "cwd": plan.cwd,
        "command": plan.command,
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
                "runner_id": plan.runner.id,
                "cwd": plan.cwd,
                "command": plan.command,
                "capture_patch": request.capture_patch,
                "job_id": job.job_id(),
                "source_snapshot": source_snapshot,
            }))?;
            let baseline = if request.capture_patch {
                Some(capture_baseline(&plan.cwd)?)
            } else {
                None
            };
            let process_output =
                execute_runner_process_until_cancelled(&plan, || job.is_cancelled())?;
            let stdout = process_output.stdout.clone();
            let stderr = process_output.stderr.clone();
            let exit_code = process_output.exit_code;
            let metrics = process_output.metrics.clone();
            let capture = process_output.capture.clone();
            if job.is_cancelled() {
                let _ = job.progress(json!({
                    "phase": "cancelled",
                    "exit_code": exit_code,
                    "metrics": metrics.clone(),
                }));
                return Ok(json!({
                    "runner_id": plan.runner.id.clone(),
                    "cwd": plan.cwd.clone(),
                    "command": plan.command.clone(),
                    "exit_code": exit_code,
                    "stdout": stdout,
                    "stderr": stderr,
                    "source_snapshot": source_snapshot,
                    "metrics": metrics,
                    "capture": capture,
                    "status": JobStatus::Cancelled,
                }));
            }
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
                    &plan.runner.id,
                    &plan.cwd,
                    &plan.command,
                    source_snapshot.as_ref(),
                    &baseline,
                    exit_code,
                )?)
            } else {
                None
            };
            let result = json!({
                "runner_id": plan.runner.id,
                "cwd": plan.cwd,
                "command": plan.command,
                "exit_code": exit_code,
                "stdout": stdout,
                "stderr": stderr,
                "source_snapshot": source_snapshot,
                "patch": patch,
                "metrics": metrics,
                "capture": capture,
            });
            if exit_code != 0 {
                job.result(result.clone())?;
                return Err(Error::remote_command_failed(RemoteCommandFailedDetails {
                    command: plan.command.join(" "),
                    exit_code,
                    stdout,
                    stderr,
                    target: TargetDetails {
                        project_id: None,
                        server_id: Some(plan.runner.id.clone()),
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

fn daemon_runtime_snapshot() -> &'static DaemonRuntimeSnapshot {
    DAEMON_RUNTIME_SNAPSHOT.get_or_init(capture_daemon_runtime_snapshot)
}

fn daemon_runtime_paths_body() -> serde_json::Value {
    let snapshot = daemon_runtime_snapshot();
    let current: Vec<_> = snapshot
        .paths
        .iter()
        .map(|path| DaemonRuntimePathSnapshot {
            env: path.env.clone(),
            path: path.path.clone(),
            fingerprint: runtime_path_fingerprint(Path::new(&path.path)),
        })
        .collect();
    let stale: Vec<_> = snapshot
        .paths
        .iter()
        .zip(current.iter())
        .filter(|(loaded, current)| loaded.fingerprint != current.fingerprint)
        .map(|(loaded, current)| {
            json!({
                "env": loaded.env,
                "path": loaded.path,
                "loaded_fingerprint": loaded.fingerprint,
                "current_fingerprint": current.fingerprint,
            })
        })
        .collect();

    json!({
        "loaded_at": snapshot.loaded_at,
        "loaded": snapshot.paths,
        "current": current,
        "stale": stale,
    })
}

fn capture_daemon_runtime_snapshot() -> DaemonRuntimeSnapshot {
    let paths = runtime_path_env_values()
        .into_iter()
        .map(|(env, path)| DaemonRuntimePathSnapshot {
            env,
            fingerprint: runtime_path_fingerprint(Path::new(&path)),
            path,
        })
        .collect();

    DaemonRuntimeSnapshot {
        loaded_at: chrono::Utc::now().to_rfc3339(),
        paths,
    }
}

fn runtime_path_env_values() -> BTreeMap<String, String> {
    std::env::vars()
        .filter(|(name, value)| is_runtime_path_env(name) && !value.trim().is_empty())
        .collect()
}

fn is_runtime_path_env(name: &str) -> bool {
    name.starts_with("HOMEBOY_")
        && RUNTIME_PATH_SUFFIXES
            .iter()
            .any(|suffix| name.ends_with(suffix))
}

fn runtime_path_fingerprint(path: &Path) -> String {
    let mut state = RuntimePathFingerprintState::default();
    collect_runtime_path_fingerprint(path, &mut state);
    format!(
        "exists={};files={};dirs={};bytes={};mtime_ns={};truncated={}",
        state.exists, state.files, state.dirs, state.bytes, state.latest_mtime_ns, state.truncated
    )
}

#[derive(Default)]
struct RuntimePathFingerprintState {
    exists: bool,
    files: usize,
    dirs: usize,
    bytes: u64,
    latest_mtime_ns: u128,
    truncated: bool,
}

fn collect_runtime_path_fingerprint(path: &Path, state: &mut RuntimePathFingerprintState) {
    if state.files >= RUNTIME_PATH_FILE_LIMIT {
        state.truncated = true;
        return;
    }
    let Ok(metadata) = fs::metadata(path) else {
        return;
    };
    state.exists = true;
    if metadata.is_dir() {
        state.dirs += 1;
        let Ok(entries) = fs::read_dir(path) else {
            return;
        };
        let mut entries: Vec<_> = entries.filter_map(|entry| entry.ok()).collect();
        entries.sort_by_key(|entry| entry.path());
        for entry in entries {
            let path = entry.path();
            if should_skip_runtime_fingerprint_path(&path) {
                continue;
            }
            collect_runtime_path_fingerprint(&path, state);
            if state.truncated {
                return;
            }
        }
    } else if metadata.is_file() {
        state.files += 1;
        state.bytes = state.bytes.saturating_add(metadata.len());
    }
    if let Ok(modified) = metadata.modified() {
        if let Ok(duration) = modified.duration_since(UNIX_EPOCH) {
            state.latest_mtime_ns = state.latest_mtime_ns.max(duration.as_nanos());
        }
    }
}

fn should_skip_runtime_fingerprint_path(path: &Path) -> bool {
    matches!(
        path.file_name().and_then(|name| name.to_str()),
        Some(".git" | "node_modules" | "vendor" | "target")
    )
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

pub(crate) fn artifact_response_for_path(path: &str) -> Option<HttpResponse> {
    artifact_download::route(path)
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
    loopback_bind: bool,
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
    let broker_token = crate::core::runner::extract_bearer_token(headers);
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
    let broker_auth = remote_runner::BrokerAuthContext {
        token: broker_token,
        loopback_bind,
        trusted_local: false,
    };
    let response = route_with_job_store_and_body_and_runner_and_auth(
        method,
        path,
        parsed_body,
        job_store,
        analysis_runner,
        broker_auth,
    );
    write_http_response(stream, response)
}

fn write_http_response(mut stream: TcpStream, response: HttpResponse) -> std::io::Result<()> {
    if let Some(artifact) = response.artifact {
        return artifact_download::write_response(stream, response.status_code, artifact);
    }

    let success = (200..300).contains(&response.status_code);
    let mut envelope = json!({
        "success": success,
        "data": response.body,
    });
    if !success {
        envelope["error"] = envelope["data"].clone();
    }
    let body = serde_json::to_string_pretty(&envelope)
        .unwrap_or_else(|_| "{\"success\":false}".to_string());
    let status_text = match response.status_code {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
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
