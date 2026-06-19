use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use reqwest::blocking::Client;
use serde::Deserialize;
use serde_json::Value;

use crate::core::api_jobs::ActiveRunnerJobSummary;
use crate::core::engine::shell;
use crate::core::error::{Error, Result};
use crate::core::paths;
use crate::core::server::{self, Server, ServerAuthMode, SshClient};

use super::session::{
    ReverseRunnerConnectOptions, RunnerConnectReport, RunnerDisconnectReport, RunnerFailureKind,
    RunnerSession, RunnerSessionRole, RunnerSessionState, RunnerStaleDaemonWarning,
    RunnerStatusReport, RunnerTunnelMode,
};
use super::{load, Runner, RunnerKind};

const REVERSE_RUNNER_HEARTBEAT_TTL: Duration = Duration::from_secs(90);

#[path = "connection_daemon.rs"]
mod connection_daemon;
use connection_daemon::{connect_remote_daemon, daemon_http_version, versions_match};
use connection_daemon::{daemon_http_identity, normalize_homeboy_version_owned};

#[derive(Debug, Clone, Deserialize)]
struct CliEnvelope {
    success: bool,
    data: Option<Value>,
    error: Option<Value>,
}

pub fn connect(runner_id: &str) -> Result<(RunnerConnectReport, i32)> {
    let runner = load(runner_id)?;
    let session_path = session_path(runner_id)?;
    let homeboy = runner.settings.homeboy_path.as_deref().unwrap_or("homeboy");

    let Some((server_id, server, client)) = resolve_ssh_runner(&runner)? else {
        return Ok(failed_connect(
            runner_id,
            session_path,
            RunnerFailureKind::SshFailure,
            "only SSH runners are supported by direct runner connect in this wave".to_string(),
        ));
    };

    let ssh_probe = client.execute("true");
    if !ssh_probe.success {
        return Ok(failed_connect(
            runner_id,
            session_path,
            RunnerFailureKind::SshFailure,
            command_failure_message("SSH connectivity check failed", &ssh_probe),
        ));
    }

    let identity = remote_homeboy_identity(&client, homeboy);
    let Ok(identity) = identity else {
        return Ok(failed_connect(
            runner_id,
            session_path,
            RunnerFailureKind::MissingRemoteHomeboy,
            identity.err().unwrap(),
        ));
    };
    let version = identity.version.clone();

    let daemon = ensure_remote_daemon(&client, homeboy);
    let Ok(daemon) = daemon else {
        return Ok(failed_connect(
            runner_id,
            session_path,
            RunnerFailureKind::DaemonStartupFailure,
            daemon.err().unwrap(),
        ));
    };

    let (local_port, tunnel_pid, local_url, daemon) = match connect_remote_daemon(
        &server,
        &client,
        homeboy,
        daemon,
        &version,
        &identity.display,
        runner_id,
        &session_path,
    ) {
        Ok(connection) => connection,
        Err(report) => return Ok(report),
    };

    let session = RunnerSession {
        runner_id: runner.id.clone(),
        mode: RunnerTunnelMode::DirectSsh,
        role: RunnerSessionRole::Controller,
        server_id: Some(server_id),
        controller_id: None,
        broker_url: None,
        remote_daemon_address: Some(daemon.address),
        local_port: Some(local_port),
        local_url: Some(local_url),
        tunnel_pid,
        remote_daemon_pid: daemon.pid,
        homeboy_version: version,
        homeboy_build_identity: Some(identity.display),
        connected_at: Utc::now().to_rfc3339(),
        worker_identity: None,
        worker_pid: None,
        last_seen_at: None,
    };
    write_session(&session)?;

    Ok((
        RunnerConnectReport {
            runner_id: runner.id,
            mode: Some(session.mode.clone()),
            role: Some(session.role.clone()),
            connected: true,
            recorded: None,
            local_url: session.local_url.clone(),
            broker_url: None,
            controller_id: None,
            remote_daemon_address: session.remote_daemon_address.clone(),
            tunnel_pid: session.tunnel_pid,
            remote_daemon_pid: session.remote_daemon_pid,
            homeboy_version: Some(session.homeboy_version.clone()),
            homeboy_build_identity: session.homeboy_build_identity.clone(),
            session_path: Some(session_path.display().to_string()),
            failure_kind: None,
            failure_message: None,
        },
        0,
    ))
}

pub fn connect_reverse(options: ReverseRunnerConnectOptions) -> Result<(RunnerConnectReport, i32)> {
    if options.runner_id.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "runner",
            "Reverse runner connect requires --reverse-runner <runner-id>",
            None,
            None,
        ));
    }
    if options.controller_id.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "controller",
            "Reverse runner connect requires a controller or broker ID",
            None,
            None,
        ));
    }

    let runner = load(&options.runner_id)?;
    let session_path = session_path(&runner.id)?;
    let homeboy_identity = crate::core::build_identity::current();
    let homeboy_version = homeboy_identity.version.clone();
    let now = Utc::now().to_rfc3339();
    let session = RunnerSession {
        runner_id: runner.id.clone(),
        mode: RunnerTunnelMode::Reverse,
        role: RunnerSessionRole::Runner,
        server_id: runner.server_id.clone(),
        controller_id: Some(options.controller_id.clone()),
        broker_url: options.broker_url.clone(),
        remote_daemon_address: None,
        local_port: None,
        local_url: None,
        tunnel_pid: None,
        remote_daemon_pid: None,
        homeboy_version,
        homeboy_build_identity: Some(homeboy_identity.display),
        connected_at: now.clone(),
        worker_identity: Some(format!("{}@{}", std::process::id(), hostname_fallback())),
        worker_pid: Some(std::process::id()),
        last_seen_at: Some(now),
    };
    write_session(&session)?;
    let broker_registered = match session.broker_url.as_deref() {
        Some(broker_url) => register_reverse_session_with_broker(broker_url, &session)?,
        None => false,
    };

    Ok((
        RunnerConnectReport {
            runner_id: runner.id,
            mode: Some(RunnerTunnelMode::Reverse),
            role: Some(RunnerSessionRole::Runner),
            connected: broker_registered,
            recorded: Some(true),
            local_url: None,
            broker_url: options.broker_url,
            controller_id: Some(options.controller_id),
            remote_daemon_address: None,
            tunnel_pid: None,
            remote_daemon_pid: None,
            homeboy_version: Some(session.homeboy_version),
            homeboy_build_identity: session.homeboy_build_identity,
            session_path: Some(session_path.display().to_string()),
            failure_kind: None,
            failure_message: None,
        },
        0,
    ))
}

fn register_reverse_session_with_broker(broker_url: &str, session: &RunnerSession) -> Result<bool> {
    let client = Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|err| Error::internal_unexpected(format!("build broker HTTP client: {err}")))?;
    let response = client
        .post(format!(
            "{}/runner/sessions",
            broker_url.trim_end_matches('/')
        ))
        .json(&serde_json::json!({
            "runner_id": session.runner_id,
            "controller_id": session.controller_id,
            "broker_url": session.broker_url,
            "homeboy_version": session.homeboy_version,
            "homeboy_build_identity": session.homeboy_build_identity,
            "worker_identity": session.worker_identity,
            "worker_pid": session.worker_pid,
            "last_seen_at": session.last_seen_at,
        }))
        .send()
        .map_err(|err| {
            Error::internal_unexpected(format!("register reverse runner session: {err}"))
        })?;
    let status_code = response.status().as_u16();
    let envelope: CliEnvelope = response.json().map_err(|err| {
        Error::internal_json(
            err.to_string(),
            Some("parse reverse runner session registration response".to_string()),
        )
    })?;
    if status_code >= 400 || !envelope.success {
        return Err(Error::internal_unexpected(format!(
            "reverse runner session registration failed: {}",
            envelope.error.unwrap_or(Value::Null)
        )));
    }
    Ok(true)
}

pub fn status(runner_id: &str) -> Result<RunnerStatusReport> {
    let runner = load(runner_id)?;
    let session_path = session_path(runner_id)?;
    let session = read_session(runner_id)?;
    let state = session_state(session.as_ref());
    let connected = state == RunnerSessionState::Connected;
    let stale_daemon = stale_daemon_warning(&runner, session.as_ref(), connected);
    let active_jobs = if connected {
        active_runner_jobs(runner_id)
    } else {
        Vec::new()
    };
    let active_job_count = active_jobs.len();
    Ok(RunnerStatusReport {
        runner_id: runner_id.to_string(),
        connected,
        state,
        session,
        stale_daemon,
        active_jobs,
        active_job_count,
        session_path: session_path.display().to_string(),
    })
}

fn active_runner_jobs(runner_id: &str) -> Vec<ActiveRunnerJobSummary> {
    super::daemon_api_get(runner_id, "/jobs")
        .ok()
        .and_then(|data| data.get("body").cloned())
        .and_then(|body| body.get("active_runner_jobs").cloned())
        .and_then(|jobs| serde_json::from_value(jobs).ok())
        .map(|jobs: Vec<ActiveRunnerJobSummary>| {
            jobs.into_iter()
                .filter(|job| job.runner_id == runner_id)
                .collect()
        })
        .unwrap_or_default()
}

fn stale_daemon_warning(
    runner: &Runner,
    session: Option<&RunnerSession>,
    connected: bool,
) -> Option<RunnerStaleDaemonWarning> {
    if !connected || runner.kind != RunnerKind::Ssh {
        return None;
    }
    let session = session?;
    if session.mode != RunnerTunnelMode::DirectSsh {
        return None;
    }
    let homeboy = runner.settings.homeboy_path.as_deref().unwrap_or("homeboy");
    let (_server_id, _server, client) = resolve_ssh_runner(runner).ok()??;
    let current_identity = remote_homeboy_identity(&client, homeboy).ok()?;
    let current_version = current_identity.version.clone();
    let observed_session_version = session
        .local_url
        .as_deref()
        .and_then(|local_url| daemon_http_version(local_url).ok())
        .unwrap_or_else(|| session.homeboy_version.clone());
    let daemon_identity = session
        .local_url
        .as_deref()
        .and_then(|local_url| daemon_http_identity(local_url).ok())
        .filter(|identity| !identity.trim().is_empty());
    let session_identity = daemon_identity.or_else(|| session.homeboy_build_identity.clone());
    if versions_match(&observed_session_version, &current_version)
        && versions_match(&session.homeboy_version, &current_version)
        && identities_match(session_identity.as_deref(), Some(&current_identity.display))
    {
        return None;
    }
    Some(RunnerStaleDaemonWarning::new(
        &runner.id,
        observed_session_version,
        current_version,
        session_identity,
        Some(current_identity.display),
    ))
}

pub fn statuses() -> Result<Vec<RunnerStatusReport>> {
    let mut reports = Vec::new();
    for runner in super::list()? {
        reports.push(status(&runner.id)?);
    }
    Ok(reports)
}

pub fn disconnect(runner_id: &str) -> Result<RunnerDisconnectReport> {
    load(runner_id)?;
    let session_path = session_path(runner_id)?;
    let session = read_session(runner_id)?;
    if let Some(session) = &session {
        if let Some(pid) = session.tunnel_pid {
            terminate_pid(pid);
        }
    }
    if session_path.exists() {
        std::fs::remove_file(&session_path).map_err(|err| {
            Error::internal_io(
                err.to_string(),
                Some(format!("delete {}", session_path.display())),
            )
        })?;
    }
    Ok(RunnerDisconnectReport {
        runner_id: runner_id.to_string(),
        disconnected: session.is_some(),
        session,
        session_path: session_path.display().to_string(),
    })
}

fn resolve_ssh_runner(runner: &Runner) -> Result<Option<(String, Server, SshClient)>> {
    if runner.kind != RunnerKind::Ssh {
        return Ok(None);
    }
    let server_id = runner.server_id.clone().ok_or_else(|| {
        Error::validation_invalid_argument(
            "server_id",
            "SSH runner requires server_id",
            Some(runner.id.clone()),
            None,
        )
    })?;
    let server = server::load(&server_id)?;
    let mut client = SshClient::from_server(&server, &server_id)?;
    client.env.extend(runner.env.clone());
    Ok(Some((server_id, server, client)))
}

fn remote_homeboy_version(
    client: &SshClient,
    homeboy: &str,
) -> std::result::Result<String, String> {
    let command = format!("{} --version", shell::quote_arg(homeboy));
    let output = client.execute(&command);
    if !output.success {
        return Err(command_failure_message(
            "remote Homeboy version check failed",
            &output,
        ));
    }
    let version = output.stdout.trim().to_string();
    if version.is_empty() {
        return Err("remote Homeboy version check returned empty output".to_string());
    }
    Ok(version)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RemoteHomeboyIdentity {
    version: String,
    display: String,
}

fn remote_homeboy_identity(
    client: &SshClient,
    homeboy: &str,
) -> std::result::Result<RemoteHomeboyIdentity, String> {
    let command = format!("{} self identity", shell::quote_arg(homeboy));
    let output = client.execute(&command);
    if output.success {
        if let Some(identity) = parse_self_identity_output(&output.stdout) {
            return Ok(identity);
        }
    }

    let version = remote_homeboy_version(client, homeboy)?;
    Ok(RemoteHomeboyIdentity {
        version: normalize_homeboy_version_owned(&version),
        display: version,
    })
}

fn parse_self_identity_output(output: &str) -> Option<RemoteHomeboyIdentity> {
    let body: Value = serde_json::from_str(output.trim()).ok()?;
    let data = body.get("data").unwrap_or(&body);
    let version = data.get("version")?.as_str()?.trim();
    let display = data
        .get("display")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(version);
    if version.is_empty() {
        return None;
    }
    Some(RemoteHomeboyIdentity {
        version: version.to_string(),
        display: display.to_string(),
    })
}

fn identities_match(left: Option<&str>, right: Option<&str>) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => versions_match(left, right),
        _ => false,
    }
}

pub(super) struct SshTunnelOutput {
    pub(super) pid: Option<u32>,
    pub(super) stderr: String,
    pub(super) success: bool,
}

pub(super) fn open_loopback_tunnel(
    server: &Server,
    local_port: u16,
    remote_host: &str,
    remote_port: u16,
) -> SshTunnelOutput {
    if is_loopback_host(&server.host) {
        return SshTunnelOutput {
            pid: None,
            stderr: String::new(),
            success: true,
        };
    }

    let mut args = Vec::new();
    if let Some(identity_file) = server
        .identity_file
        .as_deref()
        .filter(|path| !path.is_empty())
    {
        args.push("-i".to_string());
        args.push(shellexpand::tilde(identity_file).to_string());
    }
    if server.port != 22 {
        args.push("-p".to_string());
        args.push(server.port.to_string());
    }
    if let Some(auth) = &server.auth {
        if auth.mode == ServerAuthMode::KeyPlusPasswordControlmaster {
            let control_path = auth
                .session
                .control_path
                .as_deref()
                .unwrap_or("~/.ssh/controlmasters/%h-%p-%r");
            let persist = auth.session.persist.as_deref().unwrap_or("4h");
            args.extend([
                "-o".to_string(),
                "ControlMaster=auto".to_string(),
                "-o".to_string(),
                format!("ControlPath={}", shellexpand::tilde(control_path)),
                "-o".to_string(),
                format!("ControlPersist={}", persist),
            ]);
        }
    }
    args.extend([
        "-o".to_string(),
        "BatchMode=yes".to_string(),
        "-o".to_string(),
        "ExitOnForwardFailure=yes".to_string(),
        "-o".to_string(),
        "ConnectTimeout=10".to_string(),
        "-N".to_string(),
        "-L".to_string(),
        format!("127.0.0.1:{}:{}:{}", local_port, remote_host, remote_port),
        format!("{}@{}", server.user, server.host),
    ]);

    let child = std::process::Command::new("ssh")
        .args(&args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    match child {
        Ok(child) => SshTunnelOutput {
            pid: Some(child.id()),
            stderr: String::new(),
            success: true,
        },
        Err(err) => SshTunnelOutput {
            pid: None,
            stderr: format!("SSH tunnel error: {}", err),
            success: false,
        },
    }
}

#[derive(Debug)]
pub(super) struct RemoteDaemon {
    pub(super) address: String,
    pub(super) pid: Option<u32>,
}

fn ensure_remote_daemon(
    client: &SshClient,
    homeboy: &str,
) -> std::result::Result<RemoteDaemon, String> {
    if let Some(daemon) = remote_daemon_status(client, homeboy)? {
        if let Some(stale_reason) = remote_daemon_binary_stale(client, homeboy, &daemon)? {
            log_status!(
                "runner",
                "Remote managed daemon is stale ({stale_reason}); restarting it"
            );
            remote_daemon_stop(client, homeboy)?;
            return remote_daemon_start(client, homeboy);
        }
        return Ok(daemon);
    }
    remote_daemon_start(client, homeboy)
}

fn remote_daemon_binary_stale(
    client: &SshClient,
    homeboy: &str,
    daemon: &RemoteDaemon,
) -> std::result::Result<Option<String>, String> {
    let Some(pid) = daemon.pid else {
        return Ok(None);
    };
    let command = remote_daemon_binary_probe_command(homeboy, pid);
    let output = client.execute(&command);
    match classify_remote_daemon_binary_probe(output.exit_code, &output.stdout) {
        RemoteDaemonBinaryProbe::Fresh | RemoteDaemonBinaryProbe::Unknown => Ok(None),
        RemoteDaemonBinaryProbe::Stale(reason) => Ok(Some(reason)),
        RemoteDaemonBinaryProbe::Failed => Err(command_failure_message(
            "remote daemon binary freshness probe failed",
            &output,
        )),
    }
}

fn remote_daemon_stop(client: &SshClient, homeboy: &str) -> std::result::Result<(), String> {
    let command = format!("{} daemon stop", shell::quote_arg(homeboy));
    let output = client.execute(&command);
    if !output.success {
        return Err(command_failure_message(
            "remote daemon stop failed while refreshing stale managed daemon",
            &output,
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RemoteDaemonBinaryProbe {
    Fresh,
    Stale(String),
    Unknown,
    Failed,
}

fn classify_remote_daemon_binary_probe(exit_code: i32, stdout: &str) -> RemoteDaemonBinaryProbe {
    match exit_code {
        0 => RemoteDaemonBinaryProbe::Fresh,
        10 => RemoteDaemonBinaryProbe::Stale(stdout.trim().to_string()),
        2 => RemoteDaemonBinaryProbe::Unknown,
        _ => RemoteDaemonBinaryProbe::Failed,
    }
}

fn remote_daemon_binary_probe_command(homeboy: &str, pid: u32) -> String {
    let quoted_homeboy = shell::quote_arg(homeboy);
    format!(
        r#"set -eu
pid={pid}
current=$(command -v {quoted_homeboy} 2>/dev/null || printf '%s\n' {quoted_homeboy})
current=$(readlink -f "$current" 2>/dev/null || printf '%s\n' "$current")
if [ ! -e "/proc/$pid/exe" ]; then
  exit 2
fi
exe=$(readlink "/proc/$pid/exe" 2>/dev/null || true)
if [ -z "$exe" ]; then
  exit 2
fi
case "$exe" in
  *" (deleted)")
    printf 'daemon pid %s executable has been replaced: %s\n' "$pid" "$exe"
    exit 10
    ;;
esac
current_id=$(stat -Lc '%d:%i' "$current" 2>/dev/null || true)
daemon_id=$(stat -Lc '%d:%i' "/proc/$pid/exe" 2>/dev/null || true)
if [ -z "$current_id" ] || [ -z "$daemon_id" ]; then
  exit 2
fi
if [ "$current_id" != "$daemon_id" ]; then
  printf 'daemon pid %s executable inode differs from current Homeboy binary %s\n' "$pid" "$current"
  exit 10
fi
exit 0"#
    )
}

fn remote_daemon_status(
    client: &SshClient,
    homeboy: &str,
) -> std::result::Result<Option<RemoteDaemon>, String> {
    let command = format!("{} daemon status", shell::quote_arg(homeboy));
    let output = client.execute(&command);
    if !output.success {
        return Ok(None);
    }
    let envelope = parse_envelope(&output.stdout)
        .map_err(|err| format!("remote daemon status returned invalid JSON: {}", err))?;
    if !envelope.success {
        return Ok(None);
    }
    let Some(data) = envelope.data else {
        return Ok(None);
    };
    if !data
        .get("running")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Ok(None);
    }
    let Some(state) = data.get("state") else {
        return Ok(None);
    };
    Ok(Some(RemoteDaemon {
        address: state
            .get("address")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        pid: state
            .get("pid")
            .and_then(Value::as_u64)
            .and_then(|pid| u32::try_from(pid).ok()),
    }))
}

pub(super) fn remote_daemon_start(
    client: &SshClient,
    homeboy: &str,
) -> std::result::Result<RemoteDaemon, String> {
    let command = format!(
        "{} daemon start --addr 127.0.0.1:0",
        shell::quote_arg(homeboy)
    );
    let output = client.execute(&command);
    if !output.success {
        return Err(command_failure_message(
            "remote daemon startup failed",
            &output,
        ));
    }
    let envelope = parse_envelope(&output.stdout)
        .map_err(|err| format!("remote daemon start returned invalid JSON: {}", err))?;
    if !envelope.success {
        return Err(format!(
            "remote daemon start failed: {}",
            envelope.error.unwrap_or(Value::Null)
        ));
    }
    let data = envelope
        .data
        .ok_or_else(|| "remote daemon start returned no data".to_string())?;
    Ok(RemoteDaemon {
        address: data
            .get("address")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        pid: data
            .get("pid")
            .and_then(Value::as_u64)
            .and_then(|pid| u32::try_from(pid).ok()),
    })
}

fn parse_envelope(stdout: &str) -> serde_json::Result<CliEnvelope> {
    serde_json::from_str(stdout.trim())
}

pub(super) fn parse_loopback_daemon_addr(address: &str) -> std::result::Result<SocketAddr, ()> {
    let addr: SocketAddr = address.parse().map_err(|_| ())?;
    if addr.ip().is_loopback() {
        Ok(addr)
    } else {
        Err(())
    }
}

pub(super) fn reserve_loopback_port() -> Result<u16> {
    let listener = TcpListener::bind((IpAddr::from([127, 0, 0, 1]), 0)).map_err(|err| {
        Error::internal_io(
            err.to_string(),
            Some("reserve local tunnel port".to_string()),
        )
    })?;
    let port = listener
        .local_addr()
        .map_err(|err| {
            Error::internal_io(err.to_string(), Some("read local tunnel port".to_string()))
        })?
        .port();
    drop(listener);
    Ok(port)
}

pub(super) fn wait_for_tcp(port: u16, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return true;
        }
        thread::sleep(Duration::from_millis(50));
    }
    false
}

fn session_is_live(session: &RunnerSession) -> bool {
    if session.mode != RunnerTunnelMode::DirectSsh {
        return false;
    }
    if let Some(pid) = session.tunnel_pid {
        if !crate::core::process::pid_is_running(pid) {
            return false;
        }
    }
    session
        .local_port
        .is_some_and(|port| wait_for_tcp(port, Duration::from_millis(200)))
}

fn reverse_controller_session_is_live(session: &RunnerSession) -> bool {
    let Some(last_seen_at) = session.last_seen_at.as_deref() else {
        return false;
    };
    let Ok(last_seen_at) = DateTime::parse_from_rfc3339(last_seen_at) else {
        return false;
    };
    let age = Utc::now().signed_duration_since(last_seen_at.with_timezone(&Utc));
    match age.to_std() {
        Ok(age) => age <= REVERSE_RUNNER_HEARTBEAT_TTL,
        Err(_) => true,
    }
}

fn session_state(session: Option<&RunnerSession>) -> RunnerSessionState {
    match session {
        Some(session)
            if session.mode == RunnerTunnelMode::Reverse
                && session.role == RunnerSessionRole::Controller =>
        {
            if reverse_controller_session_is_live(session) {
                RunnerSessionState::Connected
            } else {
                RunnerSessionState::Recorded
            }
        }
        Some(session) if session.mode == RunnerTunnelMode::Reverse => RunnerSessionState::Recorded,
        Some(session) if session_is_live(session) => RunnerSessionState::Connected,
        Some(_) => RunnerSessionState::Disconnected,
        None => RunnerSessionState::Disconnected,
    }
}

fn hostname_fallback() -> String {
    std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .unwrap_or_else(|_| "unknown-host".to_string())
}

fn session_path(runner_id: &str) -> Result<PathBuf> {
    paths::runner_session_file(runner_id)
}

fn read_session(runner_id: &str) -> Result<Option<RunnerSession>> {
    let path = session_path(runner_id)?;
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(&path).map_err(|err| {
        Error::internal_io(err.to_string(), Some(format!("read {}", path.display())))
    })?;
    serde_json::from_str(&raw)
        .map(Some)
        .map_err(|err| Error::config_invalid_json(path.display().to_string(), err))
}

fn write_session(session: &RunnerSession) -> Result<()> {
    let path = session_path(&session.runner_id)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|err| {
            Error::internal_io(
                err.to_string(),
                Some(format!("create {}", parent.display())),
            )
        })?;
    }
    let body = serde_json::to_string_pretty(session).map_err(|err| {
        Error::internal_json(
            err.to_string(),
            Some("serialize runner session".to_string()),
        )
    })?;
    std::fs::write(&path, body).map_err(|err| {
        Error::internal_io(err.to_string(), Some(format!("write {}", path.display())))
    })
}

pub(super) fn failed_connect(
    runner_id: &str,
    session_path: PathBuf,
    failure_kind: RunnerFailureKind,
    failure_message: String,
) -> (RunnerConnectReport, i32) {
    (
        RunnerConnectReport {
            runner_id: runner_id.to_string(),
            mode: None,
            role: None,
            connected: false,
            recorded: None,
            local_url: None,
            broker_url: None,
            controller_id: None,
            remote_daemon_address: None,
            tunnel_pid: None,
            remote_daemon_pid: None,
            homeboy_version: None,
            homeboy_build_identity: None,
            session_path: Some(session_path.display().to_string()),
            failure_kind: Some(failure_kind),
            failure_message: Some(failure_message),
        },
        20,
    )
}

pub(super) fn command_failure_message(
    prefix: &str,
    output: &crate::core::server::CommandOutput,
) -> String {
    format!(
        "{} (exit {}): stdout={}, stderr={}",
        prefix,
        output.exit_code,
        output.stdout.trim(),
        output.stderr.trim()
    )
}

fn is_loopback_host(host: &str) -> bool {
    matches!(host, "localhost" | "127.0.0.1" | "::1")
}

pub(super) fn terminate_pid(pid: u32) {
    if pid > i32::MAX as u32 {
        return;
    }
    #[cfg(unix)]
    unsafe {
        let _ = libc::kill(pid as libc::pid_t, libc::SIGTERM);
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::connection_daemon::{
        daemon_identity_from_body, daemon_version_from_body, versions_match,
    };
    use super::*;
    use crate::test_support;

    #[test]
    fn rejects_non_loopback_remote_daemon_address() {
        assert!(parse_loopback_daemon_addr("0.0.0.0:1234").is_err());
        assert!(parse_loopback_daemon_addr("127.0.0.1:1234").is_ok());
    }

    #[test]
    fn parses_daemon_status_envelope() {
        let envelope = parse_envelope(
            r#"{"success":true,"data":{"action":"status","running":true,"state":{"address":"127.0.0.1:49152","pid":123}}}"#,
        )
        .expect("parse envelope");

        assert!(envelope.success);
        assert_eq!(
            envelope
                .data
                .unwrap()
                .get("state")
                .unwrap()
                .get("address")
                .unwrap(),
            "127.0.0.1:49152"
        );
    }

    #[test]
    fn compares_cli_and_daemon_version_shapes() {
        assert!(versions_match("homeboy 0.204.0", "0.204.0"));
        assert!(versions_match("0.204.0", "homeboy 0.204.0"));
        assert!(versions_match(
            "homeboy 0.204.0+19a41cd5102d",
            "0.204.0+19a41cd5102d"
        ));
        assert!(!versions_match("homeboy 0.201.3", "homeboy 0.204.0"));
    }

    #[test]
    fn extracts_current_and_legacy_daemon_version_shapes() {
        assert_eq!(
            daemon_version_from_body(&serde_json::json!({"version":"0.204.0"})),
            Some("0.204.0")
        );
        assert_eq!(
            daemon_version_from_body(
                &serde_json::json!({"success":true,"data":{"version":"0.199.4"}})
            ),
            Some("0.199.4")
        );
        assert_eq!(
            daemon_identity_from_body(
                &serde_json::json!({"version":"0.228.13","build_identity":{"display":"homeboy 0.228.13+f7569a5e"}})
            ),
            Some("homeboy 0.228.13+f7569a5e")
        );
        assert_eq!(
            daemon_identity_from_body(&serde_json::json!({"version":"0.228.13"})),
            None
        );
    }

    #[test]
    fn parses_self_identity_json_envelope() {
        let identity = parse_self_identity_output(
            r#"{"success":true,"data":{"version":"0.228.13","display":"homeboy 0.228.13+19a41cd5102d"}}"#,
        )
        .expect("identity");

        assert_eq!(identity.version, "0.228.13");
        assert_eq!(identity.display, "homeboy 0.228.13+19a41cd5102d");
    }

    #[test]
    fn stale_daemon_warning_includes_ordered_restart_recovery_commands() {
        let warning = RunnerStaleDaemonWarning::new(
            "homeboy-lab",
            "homeboy 0.201.3".to_string(),
            "homeboy 0.204.0".to_string(),
            Some("homeboy 0.201.3+old".to_string()),
            Some("homeboy 0.204.0+new".to_string()),
        );

        assert_eq!(warning.session_homeboy_version, "homeboy 0.201.3");
        assert_eq!(warning.current_homeboy_version, "homeboy 0.204.0");
        assert_eq!(
            warning.session_homeboy_build_identity.as_deref(),
            Some("homeboy 0.201.3+old")
        );
        assert!(warning.message.contains("different Homeboy build"));
        assert!(warning.message.contains("run recovery_commands in order"));
        assert_eq!(
            warning.recovery_commands,
            vec![
                "homeboy runner disconnect homeboy-lab".to_string(),
                "homeboy runner connect homeboy-lab".to_string(),
            ]
        );
    }

    #[test]
    fn classifies_remote_daemon_binary_probe_results() {
        assert_eq!(
            classify_remote_daemon_binary_probe(0, ""),
            RemoteDaemonBinaryProbe::Fresh
        );
        assert_eq!(
            classify_remote_daemon_binary_probe(2, ""),
            RemoteDaemonBinaryProbe::Unknown
        );
        assert_eq!(
            classify_remote_daemon_binary_probe(
                10,
                "daemon pid 123 executable has been replaced\n"
            ),
            RemoteDaemonBinaryProbe::Stale(
                "daemon pid 123 executable has been replaced".to_string()
            )
        );
        assert_eq!(
            classify_remote_daemon_binary_probe(1, "boom"),
            RemoteDaemonBinaryProbe::Failed
        );
    }

    #[test]
    fn remote_daemon_binary_probe_detects_deleted_or_replaced_executables() {
        let command = remote_daemon_binary_probe_command("/home/user/.cargo/bin/homeboy", 3790534);

        assert!(command.contains("/proc/$pid/exe"));
        assert!(command.contains("*\" (deleted)\")"));
        assert!(command.contains("stat -Lc '%d:%i'"));
        assert!(command.contains("executable inode differs from current Homeboy binary"));
    }

    #[test]
    fn test_open_loopback_tunnel_noops_for_local_runner() {
        let server = Server {
            id: "local".to_string(),
            aliases: Vec::new(),
            host: "127.0.0.1".to_string(),
            user: "tester".to_string(),
            port: 22,
            identity_file: None,
            kind: None,
            auth: None,
            env: HashMap::new(),
            runner: None,
        };

        let tunnel = open_loopback_tunnel(&server, 49100, "127.0.0.1", 49200);

        assert!(tunnel.success);
        assert_eq!(tunnel.pid, None);
        assert_eq!(tunnel.stderr, "");
    }

    #[test]
    fn connect_reports_local_runner_as_unsupported() {
        test_support::with_isolated_home(|_| {
            crate::core::runner::create(r#"{"id":"lab-local","kind":"local"}"#, false)
                .expect("create runner");

            let (report, exit_code) = connect("lab-local").expect("connect report");

            assert_eq!(exit_code, 20);
            assert!(!report.connected);
            assert_eq!(report.failure_kind, Some(RunnerFailureKind::SshFailure));
            assert!(report
                .failure_message
                .as_deref()
                .unwrap_or_default()
                .contains("only SSH runners"));
        });
    }

    #[test]
    fn disconnect_removes_existing_session_file() {
        test_support::with_isolated_home(|_| {
            crate::core::runner::create(r#"{"id":"lab-local","kind":"local"}"#, false)
                .expect("create runner");
            let session = RunnerSession {
                runner_id: "lab-local".to_string(),
                mode: RunnerTunnelMode::DirectSsh,
                role: RunnerSessionRole::Controller,
                server_id: None,
                controller_id: None,
                broker_url: None,
                remote_daemon_address: Some("127.0.0.1:49152".to_string()),
                local_port: Some(49153),
                local_url: Some("http://127.0.0.1:49153".to_string()),
                tunnel_pid: None,
                remote_daemon_pid: None,
                homeboy_version: "test".to_string(),
                homeboy_build_identity: Some("homeboy test+abc123".to_string()),
                connected_at: Utc::now().to_rfc3339(),
                worker_identity: None,
                worker_pid: None,
                last_seen_at: None,
            };
            write_session(&session).expect("write session");
            let path = session_path("lab-local").expect("session path");
            assert!(path.exists());

            let report = disconnect("lab-local").expect("disconnect");

            assert!(report.disconnected);
            assert_eq!(report.session.expect("session").runner_id, "lab-local");
            assert!(!path.exists());
        });
    }

    #[test]
    fn records_reverse_runner_session_without_marking_transport_live() {
        test_support::with_isolated_home(|_| {
            crate::core::runner::create(
                r#"{"id":"homeboy-lab","kind":"local","workspace_root":"/home/user/Developer"}"#,
                false,
            )
            .expect("create runner");

            let (report, exit_code) = connect_reverse(ReverseRunnerConnectOptions {
                controller_id: "extra-chill".to_string(),
                runner_id: "homeboy-lab".to_string(),
                broker_url: None,
            })
            .expect("record reverse session");

            assert_eq!(exit_code, 0);
            assert!(!report.connected);
            assert_eq!(report.recorded, Some(true));
            assert_eq!(report.mode, Some(RunnerTunnelMode::Reverse));
            assert_eq!(report.role, Some(RunnerSessionRole::Runner));
            assert_eq!(report.controller_id.as_deref(), Some("extra-chill"));

            let status = status("homeboy-lab").expect("status");
            assert!(!status.connected);
            assert_eq!(status.state, RunnerSessionState::Recorded);
            let session = status.session.expect("session");
            assert_eq!(session.mode, RunnerTunnelMode::Reverse);
            assert_eq!(session.role, RunnerSessionRole::Runner);
            assert_eq!(session.controller_id.as_deref(), Some("extra-chill"));
            assert_eq!(session.broker_url, None);
            assert_eq!(session.local_url, None);
            assert_eq!(session.local_port, None);
        });
    }

    #[test]
    fn status_lists_reverse_session_records() {
        test_support::with_isolated_home(|_| {
            crate::core::runner::create(r#"{"id":"homeboy-lab","kind":"local"}"#, false)
                .expect("create runner");
            connect_reverse(ReverseRunnerConnectOptions {
                controller_id: "extra-chill".to_string(),
                runner_id: "homeboy-lab".to_string(),
                broker_url: None,
            })
            .expect("record reverse session");

            let reports = statuses().expect("statuses");

            assert_eq!(reports.len(), 1);
            assert_eq!(reports[0].runner_id, "homeboy-lab");
            assert_eq!(reports[0].state, RunnerSessionState::Recorded);
        });
    }

    #[test]
    fn reverse_controller_session_requires_fresh_heartbeat() {
        let mut session = RunnerSession {
            runner_id: "homeboy-lab".to_string(),
            mode: RunnerTunnelMode::Reverse,
            role: RunnerSessionRole::Controller,
            server_id: None,
            controller_id: Some("extra-chill".to_string()),
            broker_url: Some("http://127.0.0.1:9876".to_string()),
            remote_daemon_address: None,
            local_port: None,
            local_url: None,
            tunnel_pid: None,
            remote_daemon_pid: None,
            homeboy_version: "test".to_string(),
            homeboy_build_identity: Some("homeboy test+abc123".to_string()),
            connected_at: Utc::now().to_rfc3339(),
            worker_identity: Some("worker-1".to_string()),
            worker_pid: Some(1234),
            last_seen_at: Some(Utc::now().to_rfc3339()),
        };

        assert_eq!(session_state(Some(&session)), RunnerSessionState::Connected);

        session.last_seen_at = Some((Utc::now() - chrono::Duration::seconds(120)).to_rfc3339());
        assert_eq!(session_state(Some(&session)), RunnerSessionState::Recorded);

        session.last_seen_at = None;
        assert_eq!(session_state(Some(&session)), RunnerSessionState::Recorded);
    }
}
