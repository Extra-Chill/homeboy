use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::core::engine::shell;
use crate::core::error::{Error, Result};
use crate::core::paths;
use crate::core::server::{self, Server, ServerAuthMode, SshClient};

use super::{load, Runner, RunnerKind};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunnerSession {
    pub runner_id: String,
    pub server_id: Option<String>,
    pub remote_daemon_address: String,
    pub local_port: u16,
    pub local_url: String,
    pub tunnel_pid: Option<u32>,
    pub remote_daemon_pid: Option<u32>,
    pub homeboy_version: String,
    pub connected_at: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunnerFailureKind {
    SshFailure,
    MissingRemoteHomeboy,
    DaemonStartupFailure,
    TunnelFailure,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunnerConnectReport {
    pub runner_id: String,
    pub connected: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_daemon_address: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tunnel_pid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_daemon_pid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub homeboy_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_kind: Option<RunnerFailureKind>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_message: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunnerStatusReport {
    pub runner_id: String,
    pub connected: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session: Option<RunnerSession>,
    pub session_path: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunnerDisconnectReport {
    pub runner_id: String,
    pub disconnected: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session: Option<RunnerSession>,
    pub session_path: String,
}

#[derive(Debug, Clone, Deserialize)]
struct CliEnvelope {
    success: bool,
    data: Option<Value>,
    error: Option<Value>,
}

pub fn connect(runner_id: &str) -> Result<(RunnerConnectReport, i32)> {
    let runner = load(runner_id)?;
    let session_path = session_path(runner_id)?;
    let homeboy = runner.homeboy_path.as_deref().unwrap_or("homeboy");

    let Some((server_id, server, client)) = resolve_ssh_runner(&runner)? else {
        return Ok(failed_connect(
            runner_id,
            session_path,
            RunnerFailureKind::SshFailure,
            "only SSH runners are supported by runner connect in this wave".to_string(),
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

    let version = remote_homeboy_version(&client, homeboy);
    let Ok(version) = version else {
        return Ok(failed_connect(
            runner_id,
            session_path,
            RunnerFailureKind::MissingRemoteHomeboy,
            version.err().unwrap(),
        ));
    };

    let daemon = ensure_remote_daemon(&client, homeboy);
    let Ok(daemon) = daemon else {
        return Ok(failed_connect(
            runner_id,
            session_path,
            RunnerFailureKind::DaemonStartupFailure,
            daemon.err().unwrap(),
        ));
    };

    let Ok(remote_addr) = parse_loopback_daemon_addr(&daemon.address) else {
        return Ok(failed_connect(
            runner_id,
            session_path,
            RunnerFailureKind::DaemonStartupFailure,
            "remote daemon did not report a loopback address".to_string(),
        ));
    };

    let local_port = reserve_loopback_port()?;
    let tunnel = open_loopback_tunnel(
        &server,
        local_port,
        &remote_addr.ip().to_string(),
        remote_addr.port(),
    );
    if !tunnel.success {
        return Ok(failed_connect(
            runner_id,
            session_path,
            RunnerFailureKind::TunnelFailure,
            format!("SSH tunnel setup failed: {}", tunnel.stderr.trim()),
        ));
    }

    if !wait_for_tcp(local_port, Duration::from_secs(5)) {
        if let Some(pid) = tunnel.pid {
            terminate_pid(pid);
        }
        return Ok(failed_connect(
            runner_id,
            session_path,
            RunnerFailureKind::TunnelFailure,
            format!(
                "local tunnel 127.0.0.1:{} did not become reachable",
                local_port
            ),
        ));
    }

    let session = RunnerSession {
        runner_id: runner.id.clone(),
        server_id: Some(server_id),
        remote_daemon_address: daemon.address,
        local_port,
        local_url: format!("http://127.0.0.1:{}", local_port),
        tunnel_pid: tunnel.pid,
        remote_daemon_pid: daemon.pid,
        homeboy_version: version,
        connected_at: Utc::now().to_rfc3339(),
    };
    write_session(&session)?;

    Ok((
        RunnerConnectReport {
            runner_id: runner.id,
            connected: true,
            local_url: Some(session.local_url.clone()),
            remote_daemon_address: Some(session.remote_daemon_address.clone()),
            tunnel_pid: session.tunnel_pid,
            remote_daemon_pid: session.remote_daemon_pid,
            homeboy_version: Some(session.homeboy_version.clone()),
            session_path: Some(session_path.display().to_string()),
            failure_kind: None,
            failure_message: None,
        },
        0,
    ))
}

pub fn status(runner_id: &str) -> Result<RunnerStatusReport> {
    load(runner_id)?;
    let session_path = session_path(runner_id)?;
    let session = read_session(runner_id)?;
    let connected = session.as_ref().is_some_and(session_is_live);
    Ok(RunnerStatusReport {
        runner_id: runner_id.to_string(),
        connected,
        session,
        session_path: session_path.display().to_string(),
    })
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
    let client = SshClient::from_server(&server, &server_id)?;
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

struct SshTunnelOutput {
    pid: Option<u32>,
    stderr: String,
    success: bool,
}

fn open_loopback_tunnel(
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
struct RemoteDaemon {
    address: String,
    pid: Option<u32>,
}

fn ensure_remote_daemon(
    client: &SshClient,
    homeboy: &str,
) -> std::result::Result<RemoteDaemon, String> {
    if let Some(daemon) = remote_daemon_status(client, homeboy)? {
        return Ok(daemon);
    }
    remote_daemon_start(client, homeboy)
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

fn remote_daemon_start(
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

fn parse_loopback_daemon_addr(address: &str) -> std::result::Result<SocketAddr, ()> {
    let addr: SocketAddr = address.parse().map_err(|_| ())?;
    if addr.ip().is_loopback() {
        Ok(addr)
    } else {
        Err(())
    }
}

fn reserve_loopback_port() -> Result<u16> {
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

fn wait_for_tcp(port: u16, timeout: Duration) -> bool {
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
    if let Some(pid) = session.tunnel_pid {
        if !pid_is_running(pid) {
            return false;
        }
    }
    wait_for_tcp(session.local_port, Duration::from_millis(200))
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

fn failed_connect(
    runner_id: &str,
    session_path: PathBuf,
    failure_kind: RunnerFailureKind,
    failure_message: String,
) -> (RunnerConnectReport, i32) {
    (
        RunnerConnectReport {
            runner_id: runner_id.to_string(),
            connected: false,
            local_url: None,
            remote_daemon_address: None,
            tunnel_pid: None,
            remote_daemon_pid: None,
            homeboy_version: None,
            session_path: Some(session_path.display().to_string()),
            failure_kind: Some(failure_kind),
            failure_message: Some(failure_message),
        },
        20,
    )
}

fn command_failure_message(prefix: &str, output: &crate::core::server::CommandOutput) -> String {
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

fn pid_is_running(pid: u32) -> bool {
    if pid > i32::MAX as u32 {
        return false;
    }
    #[cfg(unix)]
    unsafe {
        libc::kill(pid as libc::pid_t, 0) == 0
    }
    #[cfg(not(unix))]
    {
        false
    }
}

fn terminate_pid(pid: u32) {
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
                server_id: None,
                remote_daemon_address: "127.0.0.1:49152".to_string(),
                local_port: 49153,
                local_url: "http://127.0.0.1:49153".to_string(),
                tunnel_pid: None,
                remote_daemon_pid: None,
                homeboy_version: "test".to_string(),
                connected_at: Utc::now().to_rfc3339(),
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
}
