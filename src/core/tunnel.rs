use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::unix::process::CommandExt;

use crate::core::config::{self, ConfigEntity};
use crate::core::error::{Error, Result};
use crate::core::paths;
use crate::core::process::{pid_is_running, process_group_is_running};
use crate::core::server;
use crate::core::{CreateOutput, MergeOutput, RemoveResult};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServiceTunnel {
    #[serde(skip)]
    pub id: String,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    pub server_id: String,
    pub target: ServiceTunnelTarget,

    #[serde(default = "default_scheme")]
    pub scheme: String,
    #[serde(default = "default_local_host")]
    pub local_host: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_port: Option<u16>,

    pub auth: ServiceTunnelAuth,
    pub policy: ServiceTunnelPolicy,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ServiceTunnelAuthMode {
    BearerEnv,
    HeaderEnv,
    BasicEnv,
    MutualTls,
    SshOnly,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServiceTunnelAuth {
    pub mode: ServiceTunnelAuthMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env_var: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub header: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServiceTunnelTarget {
    pub host: String,
    pub port: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ServiceTunnelExposure {
    PrivateLoopback,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServiceTunnelPolicy {
    #[serde(default = "default_exposure")]
    pub exposure: ServiceTunnelExposure,
    #[serde(default = "default_true")]
    pub require_auth: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_clients: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ServiceTunnelStatus {
    pub service_id: String,
    pub declared: bool,
    pub running: bool,
    pub lifecycle: String,
    pub local_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub public_url: Option<String>,
    pub remote_target: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin: Option<ServiceTunnelOriginEvidence>,
    pub policy: ServiceTunnelPolicy,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub process: Option<ServiceTunnelProcessStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub health: Option<ServiceTunnelHealthStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence: Option<ServiceTunnelEvidence>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tunnel_backend: Option<ServiceTunnelBackendStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServiceTunnelCommandSpec {
    pub command: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env_keys: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServiceTunnelRuntimeState {
    pub service_id: String,
    pub pid: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub process_group_id: Option<i32>,
    pub started_at: String,
    pub local_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub public_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub declared_service_host: Option<String>,
    pub command: ServiceTunnelCommandSpec,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub health_url: Option<String>,
    pub stdout_path: String,
    pub stderr_path: String,
    pub backend: ServiceTunnelTunnelBackend,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tunnel_process: Option<ServiceTunnelProcessRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin_evidence: Option<ServiceTunnelOriginEvidence>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ServiceTunnelTunnelBackend {
    None,
    Cloudflared,
    Command,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServiceTunnelProcessRef {
    pub pid: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub process_group_id: Option<i32>,
    pub stdout_path: String,
    pub stderr_path: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ServiceTunnelProcessStatus {
    pub pid: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub process_group_id: Option<i32>,
    pub running: bool,
    pub started_at: String,
    pub command: ServiceTunnelCommandSpec,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ServiceTunnelHealthStatus {
    pub checked: bool,
    pub healthy: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_code: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ServiceTunnelEvidence {
    pub state_path: String,
    pub stdout_path: String,
    pub stderr_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tunnel_stdout_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tunnel_stderr_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ServiceTunnelBackendStatus {
    pub backend: ServiceTunnelTunnelBackend,
    pub active: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub public_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServiceTunnelOriginEvidence {
    pub local_bind_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub declared_service_host: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub public_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effective_browser_origin: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secure_context: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_host_header: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub public_host_matches_declared_service_host: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_seen_expected_host_header: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host_header_probe_status_code: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host_header_probe_error: Option<String>,
}

pub struct StartServiceTunnelSpec {
    pub id: String,
    pub command: String,
    pub cwd: Option<PathBuf>,
    pub env: BTreeMap<String, String>,
    pub host: Option<String>,
    pub port: Option<u16>,
    pub scheme: Option<String>,
    pub health_url: Option<String>,
    pub health_path: Option<String>,
    pub readiness_timeout_secs: u64,
    pub declared_service_host: Option<String>,
    pub public_tunnel_command: Option<String>,
    pub public_tunnel_timeout_secs: u64,
    pub backend: ServiceTunnelTunnelBackend,
}

pub struct ExposeServiceTunnelSpec {
    pub id: String,
    pub server_id: String,
    pub target: ServiceTunnelTarget,
    pub scheme: String,
    pub local_port: Option<u16>,
    pub auth: ServiceTunnelAuth,
    pub policy: ServiceTunnelPolicy,
    pub description: Option<String>,
}

fn default_scheme() -> String {
    "http".to_string()
}

fn default_local_host() -> String {
    "127.0.0.1".to_string()
}

fn default_true() -> bool {
    true
}

fn default_exposure() -> ServiceTunnelExposure {
    ServiceTunnelExposure::PrivateLoopback
}

impl ConfigEntity for ServiceTunnel {
    const ENTITY_TYPE: &'static str = "service_tunnel";
    const DIR_NAME: &'static str = "service-tunnels";

    fn id(&self) -> &str {
        &self.id
    }

    fn set_id(&mut self, id: String) {
        self.id = id;
    }

    fn not_found_error(id: String, suggestions: Vec<String>) -> Error {
        Error::service_tunnel_not_found(id, suggestions)
    }

    fn config_path(id: &str) -> Result<PathBuf> {
        Ok(paths::homeboy()?
            .join("service-tunnels")
            .join(format!("{}.json", id)))
    }

    fn validate(&self) -> Result<()> {
        validate_service_tunnel(self)
    }

    fn aliases(&self) -> &[String] {
        &self.aliases
    }
}

entity_crud!(ServiceTunnel; list_ids, merge);

pub fn expose(spec: ExposeServiceTunnelSpec) -> Result<ServiceTunnel> {
    let tunnel = ServiceTunnel {
        id: spec.id,
        aliases: Vec::new(),
        description: spec.description,
        server_id: spec.server_id,
        target: spec.target,
        scheme: spec.scheme,
        local_host: default_local_host(),
        local_port: spec.local_port,
        auth: spec.auth,
        policy: spec.policy,
    };
    validate_service_tunnel(&tunnel)?;
    save(&tunnel)?;
    load(&tunnel.id)
}

pub fn status(id: &str) -> Result<ServiceTunnelStatus> {
    let tunnel = load(id)?;
    Ok(service_tunnel_status(&tunnel))
}

pub fn start(spec: StartServiceTunnelSpec) -> Result<ServiceTunnelStatus> {
    let mut tunnel = load(&spec.id)?;
    validate_backend_spec(&spec)?;

    let existing = load_runtime_state(&tunnel.id)?;
    if let Some(state) = existing {
        if runtime_state_is_running(&state) {
            return Err(Error::validation_invalid_argument(
                "service",
                "service tunnel is already running; stop it before starting again",
                Some(tunnel.id),
                None,
            ));
        }
    }

    if let Some(host) = spec.host {
        validate_loopback_host(&host, &tunnel.id)?;
        tunnel.local_host = host;
    }
    if let Some(port) = spec.port {
        if port == 0 {
            return Err(Error::validation_invalid_argument(
                "port",
                "local port must be greater than zero",
                Some(tunnel.id),
                None,
            ));
        }
        tunnel.local_port = Some(port);
    }
    if let Some(scheme) = spec.scheme {
        tunnel.scheme = scheme;
    }
    validate_service_tunnel(&tunnel)?;
    save(&tunnel)?;

    let runtime_dir = paths::service_tunnel_runtime_dir(&tunnel.id)?;
    fs::create_dir_all(&runtime_dir)
        .map_err(|e| Error::internal_io(e.to_string(), Some(runtime_dir.display().to_string())))?;
    let stdout_path = runtime_dir.join("stdout.log");
    let stderr_path = runtime_dir.join("stderr.log");
    let stdout = File::create(&stdout_path)
        .map_err(|e| Error::internal_io(e.to_string(), Some(stdout_path.display().to_string())))?;
    let stderr = File::create(&stderr_path)
        .map_err(|e| Error::internal_io(e.to_string(), Some(stderr_path.display().to_string())))?;

    let mut command = shell_command(&spec.command);
    if let Some(cwd) = &spec.cwd {
        command.current_dir(cwd);
    }
    for (key, value) in &spec.env {
        command.env(key, value);
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    #[cfg(unix)]
    unsafe {
        command.pre_exec(|| {
            libc::setpgid(0, 0);
            Ok(())
        });
    }

    let child = command.spawn().map_err(|e| {
        Error::internal_io(
            e.to_string(),
            Some(format!("start service tunnel {}", tunnel.id)),
        )
    })?;
    let pid = child.id();
    let process_group_id = process_group_id_for(pid);
    let health_url = resolve_health_url(&tunnel, spec.health_url, spec.health_path);
    let state = ServiceTunnelRuntimeState {
        service_id: tunnel.id.clone(),
        pid,
        process_group_id,
        started_at: chrono::Utc::now().to_rfc3339(),
        local_url: local_url_for(&tunnel),
        public_url: None,
        declared_service_host: spec.declared_service_host.clone(),
        command: ServiceTunnelCommandSpec {
            command: spec.command,
            cwd: spec.cwd.map(|path| path.display().to_string()),
            env_keys: spec.env.keys().cloned().collect(),
        },
        health_url,
        stdout_path: stdout_path.display().to_string(),
        stderr_path: stderr_path.display().to_string(),
        backend: spec.backend,
        tunnel_process: None,
        origin_evidence: None,
    };
    save_runtime_state(&state)?;
    if let Err(error) = wait_until_ready(&state, spec.readiness_timeout_secs) {
        terminate_runtime_state(&state)?;
        remove_runtime_state(&state.service_id)?;
        return Err(error);
    }
    let mut state = state;
    if let Err(error) = start_public_tunnel_backend(
        &mut state,
        spec.public_tunnel_command.as_deref(),
        spec.public_tunnel_timeout_secs,
    ) {
        terminate_runtime_state(&state)?;
        remove_runtime_state(&state.service_id)?;
        return Err(error);
    }
    save_runtime_state(&state)?;
    status(&tunnel.id)
}

pub fn stop(id: &str) -> Result<ServiceTunnelStatus> {
    let tunnel = load(id)?;
    if let Some(state) = load_runtime_state(id)? {
        terminate_runtime_state(&state)?;
        remove_runtime_state(id)?;
    }
    Ok(service_tunnel_status(&tunnel))
}

pub fn local_url(id: &str) -> Result<String> {
    let tunnel = load(id)?;
    Ok(local_url_for(&tunnel))
}

fn service_tunnel_status(tunnel: &ServiceTunnel) -> ServiceTunnelStatus {
    let state = load_runtime_state(&tunnel.id).ok().flatten();
    let running = state.as_ref().is_some_and(runtime_state_is_running);
    let health = state.as_ref().map(check_runtime_health);
    let evidence = state.as_ref().map(runtime_evidence);
    let process = state.as_ref().map(|state| ServiceTunnelProcessStatus {
        pid: state.pid,
        process_group_id: state.process_group_id,
        running,
        started_at: state.started_at.clone(),
        command: state.command.clone(),
    });
    let backend = state.as_ref().map(|state| ServiceTunnelBackendStatus {
        backend: state.backend.clone(),
        active: state
            .tunnel_process
            .as_ref()
            .is_some_and(tunnel_process_ref_is_running),
        pid: state.tunnel_process.as_ref().map(|process| process.pid),
        public_url: state.public_url.clone(),
    });
    let public_url = state.as_ref().and_then(|state| state.public_url.clone());
    let origin = state
        .as_ref()
        .and_then(|state| state.origin_evidence.clone());
    ServiceTunnelStatus {
        service_id: tunnel.id.clone(),
        declared: true,
        running,
        lifecycle: if running { "running" } else { "declared" }.to_string(),
        local_url: local_url_for(tunnel),
        public_url,
        remote_target: format!("{}:{}", tunnel.target.host, tunnel.target.port),
        origin,
        policy: tunnel.policy.clone(),
        process,
        health,
        evidence,
        tunnel_backend: backend,
    }
}

fn local_url_for(tunnel: &ServiceTunnel) -> String {
    match tunnel.local_port {
        Some(port) => format!("{}://{}:{}", tunnel.scheme, tunnel.local_host, port),
        None => format!("{}://{}:<auto>", tunnel.scheme, tunnel.local_host),
    }
}

fn validate_service_tunnel(tunnel: &ServiceTunnel) -> Result<()> {
    if !server::exists(&tunnel.server_id) {
        let suggestions = config::find_similar_ids::<server::Server>(&tunnel.server_id);
        return Err(Error::server_not_found(
            tunnel.server_id.clone(),
            suggestions,
        ));
    }
    if tunnel.target.host.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "target.host",
            "remote host is required",
            Some(tunnel.id.clone()),
            None,
        ));
    }
    if tunnel.target.port == 0 {
        return Err(Error::validation_invalid_argument(
            "target.port",
            "remote port must be greater than zero",
            Some(tunnel.id.clone()),
            None,
        ));
    }
    validate_loopback_host(&tunnel.local_host, &tunnel.id)?;
    if !matches!(
        tunnel.policy.exposure,
        ServiceTunnelExposure::PrivateLoopback
    ) {
        return Err(Error::validation_invalid_argument(
            "policy.exposure",
            "only private_loopback exposure is supported",
            Some(tunnel.id.clone()),
            Some(vec!["private_loopback".to_string()]),
        ));
    }
    if !tunnel.policy.require_auth {
        return Err(Error::validation_invalid_argument(
            "policy.require_auth",
            "service tunnels must require explicit auth policy",
            Some(tunnel.id.clone()),
            Some(vec!["true".to_string()]),
        ));
    }
    if matches!(
        tunnel.auth.mode,
        ServiceTunnelAuthMode::BearerEnv
            | ServiceTunnelAuthMode::HeaderEnv
            | ServiceTunnelAuthMode::BasicEnv
    ) && tunnel
        .auth
        .env_var
        .as_deref()
        .unwrap_or_default()
        .trim()
        .is_empty()
    {
        return Err(Error::validation_invalid_argument(
            "auth.env_var",
            "selected auth mode requires an environment variable name",
            Some(tunnel.id.clone()),
            None,
        ));
    }
    Ok(())
}

fn validate_backend_spec(spec: &StartServiceTunnelSpec) -> Result<()> {
    match spec.backend {
        ServiceTunnelTunnelBackend::None | ServiceTunnelTunnelBackend::Cloudflared => Ok(()),
        ServiceTunnelTunnelBackend::Command => {
            if spec
                .public_tunnel_command
                .as_deref()
                .unwrap_or_default()
                .trim()
                .is_empty()
            {
                return Err(Error::validation_invalid_argument(
                    "public_tunnel_command",
                    "command backend requires --public-tunnel-command",
                    Some(spec.id.clone()),
                    None,
                ));
            }
            Ok(())
        }
    }
}

fn validate_loopback_host(host: &str, id: &str) -> Result<()> {
    if host != "127.0.0.1" && host != "localhost" {
        return Err(Error::validation_invalid_argument(
            "local_host",
            "service tunnels may only bind to loopback hosts",
            Some(id.to_string()),
            Some(vec!["127.0.0.1".to_string(), "localhost".to_string()]),
        ));
    }
    Ok(())
}

fn shell_command(command: &str) -> Command {
    #[cfg(windows)]
    {
        let mut cmd = Command::new("cmd");
        cmd.args(["/C", command]);
        cmd
    }

    #[cfg(not(windows))]
    {
        let mut cmd = Command::new("/bin/sh");
        cmd.args(["-c", command]);
        cmd
    }
}

fn process_group_id_for(pid: u32) -> Option<i32> {
    #[cfg(unix)]
    unsafe {
        let pgid = libc::getpgid(pid as libc::pid_t);
        if pgid > 0 {
            return Some(pgid);
        }
        None
    }

    #[cfg(not(unix))]
    {
        let _ = pid;
        None
    }
}

fn load_runtime_state(id: &str) -> Result<Option<ServiceTunnelRuntimeState>> {
    let path = paths::service_tunnel_runtime_state_file(id)?;
    if !path.exists() {
        return Ok(None);
    }
    let data = fs::read_to_string(&path)
        .map_err(|e| Error::internal_io(e.to_string(), Some(path.display().to_string())))?;
    serde_json::from_str(&data)
        .map(Some)
        .map_err(|e| Error::internal_json(e.to_string(), Some(path.display().to_string())))
}

fn save_runtime_state(state: &ServiceTunnelRuntimeState) -> Result<()> {
    let path = paths::service_tunnel_runtime_state_file(&state.service_id)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| Error::internal_io(e.to_string(), Some(parent.display().to_string())))?;
    }
    let data = serde_json::to_string_pretty(state)
        .map_err(|e| Error::internal_json(e.to_string(), Some(state.service_id.clone())))?;
    fs::write(&path, data)
        .map_err(|e| Error::internal_io(e.to_string(), Some(path.display().to_string())))
}

fn remove_runtime_state(id: &str) -> Result<()> {
    let path = paths::service_tunnel_runtime_state_file(id)?;
    if path.exists() {
        fs::remove_file(&path)
            .map_err(|e| Error::internal_io(e.to_string(), Some(path.display().to_string())))?;
    }
    Ok(())
}

fn runtime_state_is_running(state: &ServiceTunnelRuntimeState) -> bool {
    if let Some(pgid) = state.process_group_id {
        process_group_is_running(pgid)
    } else {
        pid_is_running(state.pid)
    }
}

fn terminate_runtime_state(state: &ServiceTunnelRuntimeState) -> Result<()> {
    if let Some(process) = &state.tunnel_process {
        terminate_process_ref(process);
    }

    if !runtime_state_is_running(state) {
        return Ok(());
    }

    #[cfg(unix)]
    unsafe {
        if let Some(pgid) = state.process_group_id {
            libc::kill(-(pgid as libc::pid_t), libc::SIGTERM);
            std::thread::sleep(Duration::from_millis(250));
            if process_group_is_running(pgid) {
                libc::kill(-(pgid as libc::pid_t), libc::SIGKILL);
            }
        } else {
            libc::kill(state.pid as libc::pid_t, libc::SIGTERM);
        }
    }

    #[cfg(not(unix))]
    {
        let _ = state;
    }

    Ok(())
}

fn start_public_tunnel_backend(
    state: &mut ServiceTunnelRuntimeState,
    command_override: Option<&str>,
    timeout_secs: u64,
) -> Result<()> {
    if matches!(state.backend, ServiceTunnelTunnelBackend::None) {
        state.origin_evidence = Some(origin_evidence_for(state, None));
        return Ok(());
    }

    let command = public_tunnel_command(state, command_override)?;
    let runtime_dir = paths::service_tunnel_runtime_dir(&state.service_id)?;
    fs::create_dir_all(&runtime_dir)
        .map_err(|e| Error::internal_io(e.to_string(), Some(runtime_dir.display().to_string())))?;
    let stdout_path = runtime_dir.join("tunnel-stdout.log");
    let stderr_path = runtime_dir.join("tunnel-stderr.log");

    let stderr = File::create(&stderr_path)
        .map_err(|e| Error::internal_io(e.to_string(), Some(stderr_path.display().to_string())))?;
    let mut child = shell_command(&command);
    child
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::from(stderr));
    #[cfg(unix)]
    unsafe {
        child.pre_exec(|| {
            libc::setpgid(0, 0);
            Ok(())
        });
    }

    let mut child = child.spawn().map_err(|e| {
        Error::internal_io(
            e.to_string(),
            Some(format!("start public tunnel backend {}", state.service_id)),
        )
    })?;
    let pid = child.id();
    let process = ServiceTunnelProcessRef {
        pid,
        process_group_id: process_group_id_for(pid),
        stdout_path: stdout_path.display().to_string(),
        stderr_path: stderr_path.display().to_string(),
    };

    let public_url = match read_public_origin_from_child(&mut child, &stdout_path, timeout_secs) {
        Ok(url) => url,
        Err(error) => {
            terminate_process_ref(&process);
            return Err(error);
        }
    };

    state.public_url = Some(public_url.clone());
    state.tunnel_process = Some(process);
    state.origin_evidence = Some(origin_evidence_for(state, Some(&public_url)));
    std::mem::forget(child);
    Ok(())
}

fn public_tunnel_command(
    state: &ServiceTunnelRuntimeState,
    command_override: Option<&str>,
) -> Result<String> {
    match state.backend {
        ServiceTunnelTunnelBackend::None => Ok(String::new()),
        ServiceTunnelTunnelBackend::Cloudflared => Ok(format!(
            "cloudflared tunnel --url {}",
            shell_quote(&state.local_url)
        )),
        ServiceTunnelTunnelBackend::Command => command_override
            .map(str::trim)
            .filter(|command| !command.is_empty())
            .map(ToOwned::to_owned)
            .ok_or_else(|| {
                Error::validation_invalid_argument(
                    "public_tunnel_command",
                    "command backend requires --public-tunnel-command",
                    Some(state.service_id.clone()),
                    None,
                )
            }),
    }
}

fn read_public_origin_from_child(
    child: &mut Child,
    stdout_path: &PathBuf,
    timeout_secs: u64,
) -> Result<String> {
    let stdout = child.stdout.take().ok_or_else(|| {
        Error::internal_io(
            "public tunnel backend stdout was not captured".to_string(),
            Some("service_tunnel.public_tunnel.stdout".to_string()),
        )
    })?;
    let path = stdout_path.clone();
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let mut log = match File::create(&path) {
            Ok(file) => file,
            Err(error) => {
                let _ = sender.send(Err(error.to_string()));
                return;
            }
        };
        let reader = BufReader::new(stdout);
        let mut sender = Some(sender);
        for line in reader.lines() {
            match line {
                Ok(line) => {
                    let _ = writeln!(log, "{}", line);
                    if let Some(origin) = first_https_public_origin(&line) {
                        if let Some(sender) = sender.take() {
                            let _ = sender.send(Ok(origin));
                        }
                    }
                }
                Err(error) => {
                    if let Some(sender) = sender.take() {
                        let _ = sender.send(Err(error.to_string()));
                    }
                    return;
                }
            }
        }
        if let Some(sender) = sender.take() {
            let _ = sender.send(Err(
                "public tunnel backend stdout closed before an HTTPS URL appeared".to_string(),
            ));
        }
    });

    match receiver.recv_timeout(Duration::from_secs(timeout_secs.max(1))) {
        Ok(Ok(origin)) => Ok(origin),
        Ok(Err(message)) => Err(Error::validation_invalid_argument(
            "public_tunnel_backend",
            format!("public tunnel backend failed to provide an HTTPS URL: {message}"),
            None,
            None,
        )),
        Err(mpsc::RecvTimeoutError::Timeout) => Err(Error::validation_invalid_argument(
            "public_tunnel_backend",
            "public tunnel backend did not print an HTTPS URL before timeout",
            None,
            None,
        )),
        Err(mpsc::RecvTimeoutError::Disconnected) => Err(Error::validation_invalid_argument(
            "public_tunnel_backend",
            "public tunnel backend stdout reader stopped before an HTTPS URL appeared",
            None,
            None,
        )),
    }
}

fn first_https_public_origin(line: &str) -> Option<String> {
    let start = line.find("https://")?;
    let rest = &line[start..];
    let end = rest
        .find(|ch: char| ch.is_whitespace() || matches!(ch, '"' | '\'' | '<' | '>' | ')'))
        .unwrap_or(rest.len());
    public_origin_only(rest[..end].trim_end_matches('/'))
}

fn public_origin_only(url: &str) -> Option<String> {
    let parsed = reqwest::Url::parse(url).ok()?;
    let host = parsed.host_str()?;
    let port = parsed
        .port()
        .map(|port| format!(":{port}"))
        .unwrap_or_default();
    Some(format!("{}://{}{}", parsed.scheme(), host, port))
}

fn origin_evidence_for(
    state: &ServiceTunnelRuntimeState,
    public_url: Option<&str>,
) -> ServiceTunnelOriginEvidence {
    let expected_host_header = state
        .declared_service_host
        .clone()
        .or_else(|| public_url.and_then(host_from_origin));
    let probe = expected_host_header
        .as_deref()
        .map(|host| probe_expected_host_header(&state.local_url, host));
    ServiceTunnelOriginEvidence {
        local_bind_url: state.local_url.clone(),
        declared_service_host: state.declared_service_host.clone(),
        public_url: public_url.map(ToOwned::to_owned),
        effective_browser_origin: public_url.map(ToOwned::to_owned),
        secure_context: public_url.map(|url| url.starts_with("https://")),
        expected_host_header,
        public_host_matches_declared_service_host: public_url.and_then(|url| {
            state
                .declared_service_host
                .as_deref()
                .map(|declared| host_from_origin(url).as_deref() == Some(declared))
        }),
        service_seen_expected_host_header: probe.as_ref().map(|probe| probe.reached_service),
        host_header_probe_status_code: probe.as_ref().and_then(|probe| probe.status_code),
        host_header_probe_error: probe.and_then(|probe| probe.error),
    }
}

struct HostHeaderProbe {
    reached_service: bool,
    status_code: Option<u16>,
    error: Option<String>,
}

fn probe_expected_host_header(local_url: &str, expected_host: &str) -> HostHeaderProbe {
    match reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .and_then(|client| client.get(local_url).header("Host", expected_host).send())
    {
        Ok(response) => HostHeaderProbe {
            reached_service: true,
            status_code: Some(response.status().as_u16()),
            error: None,
        },
        Err(error) => HostHeaderProbe {
            reached_service: false,
            status_code: None,
            error: Some(error.to_string()),
        },
    }
}

fn host_from_origin(origin: &str) -> Option<String> {
    reqwest::Url::parse(origin)
        .ok()
        .and_then(|url| url.host_str().map(ToOwned::to_owned))
}

fn tunnel_process_ref_is_running(process: &ServiceTunnelProcessRef) -> bool {
    if let Some(pgid) = process.process_group_id {
        process_group_is_running(pgid)
    } else {
        pid_is_running(process.pid)
    }
}

fn terminate_process_ref(process: &ServiceTunnelProcessRef) {
    #[cfg(unix)]
    unsafe {
        if let Some(pgid) = process.process_group_id {
            libc::kill(-(pgid as libc::pid_t), libc::SIGTERM);
            std::thread::sleep(Duration::from_millis(250));
            if process_group_is_running(pgid) {
                libc::kill(-(pgid as libc::pid_t), libc::SIGKILL);
            }
        } else {
            libc::kill(process.pid as libc::pid_t, libc::SIGTERM);
        }
    }

    #[cfg(not(unix))]
    {
        let _ = process;
    }
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn resolve_health_url(
    tunnel: &ServiceTunnel,
    health_url: Option<String>,
    health_path: Option<String>,
) -> Option<String> {
    if let Some(url) = health_url {
        return Some(url);
    }
    health_path.map(|path| {
        let normalized = if path.starts_with('/') {
            path
        } else {
            format!("/{path}")
        };
        format!("{}{}", local_url_for(tunnel), normalized)
    })
}

fn wait_until_ready(state: &ServiceTunnelRuntimeState, timeout_secs: u64) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    loop {
        if !runtime_state_is_running(state) {
            return Err(Error::validation_invalid_argument(
                "service",
                "service process exited before becoming ready",
                Some(state.service_id.clone()),
                None,
            ));
        }
        let health = check_runtime_health(state);
        if health.healthy || (!health.checked && runtime_state_is_running(state)) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(Error::validation_invalid_argument(
                "readiness",
                "service did not become healthy before readiness timeout",
                Some(state.service_id.clone()),
                health.error.map(|error| vec![error]),
            ));
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn check_runtime_health(state: &ServiceTunnelRuntimeState) -> ServiceTunnelHealthStatus {
    let Some(url) = state.health_url.clone() else {
        return ServiceTunnelHealthStatus {
            checked: false,
            healthy: runtime_state_is_running(state),
            url: None,
            status_code: None,
            error: None,
        };
    };

    match reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .and_then(|client| client.get(&url).send())
    {
        Ok(response) => ServiceTunnelHealthStatus {
            checked: true,
            healthy: response.status().is_success(),
            url: Some(url),
            status_code: Some(response.status().as_u16()),
            error: None,
        },
        Err(error) => ServiceTunnelHealthStatus {
            checked: true,
            healthy: false,
            url: Some(url),
            status_code: None,
            error: Some(error.to_string()),
        },
    }
}

fn runtime_evidence(state: &ServiceTunnelRuntimeState) -> ServiceTunnelEvidence {
    ServiceTunnelEvidence {
        state_path: paths::service_tunnel_runtime_state_file(&state.service_id)
            .map(|path| path.display().to_string())
            .unwrap_or_default(),
        stdout_path: state.stdout_path.clone(),
        stderr_path: state.stderr_path.clone(),
        tunnel_stdout_path: state
            .tunnel_process
            .as_ref()
            .map(|process| process.stdout_path.clone()),
        tunnel_stderr_path: state
            .tunnel_process
            .as_ref()
            .map(|process| process.stderr_path.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::server::Server;
    use crate::test_support;
    use std::collections::{BTreeMap, HashMap};

    fn create_server() {
        crate::core::server::save(&Server {
            id: "private-host".to_string(),
            aliases: Vec::new(),
            host: "private.example.test".to_string(),
            user: "tester".to_string(),
            port: 22,
            identity_file: None,
            kind: None,
            auth: None,
            env: HashMap::new(),
            runner: None,
        })
        .expect("save server");
    }

    #[test]
    fn expose_records_private_loopback_declaration_without_running_tunnel() {
        test_support::with_isolated_home(|_| {
            create_server();

            let tunnel = expose(ExposeServiceTunnelSpec {
                id: "private-service".to_string(),
                server_id: "private-host".to_string(),
                target: ServiceTunnelTarget {
                    host: "127.0.0.1".to_string(),
                    port: 7331,
                },
                scheme: "http".to_string(),
                local_port: Some(8831),
                auth: ServiceTunnelAuth {
                    mode: ServiceTunnelAuthMode::BearerEnv,
                    env_var: Some("PRIVATE_SERVICE_TOKEN".to_string()),
                    header: Some("Authorization".to_string()),
                },
                policy: ServiceTunnelPolicy {
                    exposure: ServiceTunnelExposure::PrivateLoopback,
                    require_auth: true,
                    allowed_clients: vec!["runtime-client".to_string()],
                },
                description: Some("Private MCP service".to_string()),
            })
            .expect("expose service");

            assert_eq!(tunnel.id, "private-service");
            let report = status("private-service").expect("status");
            assert!(report.declared);
            assert!(!report.running);
            assert_eq!(report.local_url, "http://127.0.0.1:8831");
        });
    }

    #[test]
    fn validation_rejects_auth_mode_without_env_var() {
        test_support::with_isolated_home(|_| {
            create_server();
            let err = expose(ExposeServiceTunnelSpec {
                id: "bad".to_string(),
                server_id: "private-host".to_string(),
                target: ServiceTunnelTarget {
                    host: "127.0.0.1".to_string(),
                    port: 7331,
                },
                scheme: "http".to_string(),
                local_port: None,
                auth: ServiceTunnelAuth {
                    mode: ServiceTunnelAuthMode::BearerEnv,
                    env_var: None,
                    header: None,
                },
                policy: ServiceTunnelPolicy {
                    exposure: ServiceTunnelExposure::PrivateLoopback,
                    require_auth: true,
                    allowed_clients: Vec::new(),
                },
                description: None,
            })
            .expect_err("missing auth env should fail");

            assert_eq!(err.code, crate::core::ErrorCode::ValidationInvalidArgument);
            assert!(err.message.contains("auth.env_var"));
        });
    }

    #[test]
    fn start_status_and_stop_manage_local_service_runtime_state() {
        test_support::with_isolated_home(|_| {
            create_server();
            expose(ExposeServiceTunnelSpec {
                id: "local-preview".to_string(),
                server_id: "private-host".to_string(),
                target: ServiceTunnelTarget {
                    host: "127.0.0.1".to_string(),
                    port: 7331,
                },
                scheme: "http".to_string(),
                local_port: Some(8832),
                auth: ServiceTunnelAuth {
                    mode: ServiceTunnelAuthMode::BearerEnv,
                    env_var: Some("LOCAL_PREVIEW_TOKEN".to_string()),
                    header: Some("Authorization".to_string()),
                },
                policy: ServiceTunnelPolicy {
                    exposure: ServiceTunnelExposure::PrivateLoopback,
                    require_auth: true,
                    allowed_clients: vec!["hostname-sensitive-client".to_string()],
                },
                description: None,
            })
            .expect("expose service");

            let started = start(StartServiceTunnelSpec {
                id: "local-preview".to_string(),
                command: "while true; do sleep 1; done".to_string(),
                cwd: None,
                env: BTreeMap::from([("LOCAL_PREVIEW_MODE".to_string(), "test".to_string())]),
                host: Some("127.0.0.1".to_string()),
                port: Some(8832),
                scheme: Some("http".to_string()),
                health_url: None,
                health_path: None,
                readiness_timeout_secs: 1,
                declared_service_host: None,
                public_tunnel_command: None,
                public_tunnel_timeout_secs: 1,
                backend: ServiceTunnelTunnelBackend::None,
            })
            .expect("start service");

            assert!(started.running);
            assert_eq!(started.local_url, "http://127.0.0.1:8832");
            assert_eq!(started.public_url, None);
            let process = started.process.expect("process status");
            assert!(process.running);
            assert_eq!(process.command.env_keys, vec!["LOCAL_PREVIEW_MODE"]);
            let evidence = started.evidence.expect("evidence paths");
            assert!(std::path::Path::new(&evidence.state_path).exists());
            assert!(std::path::Path::new(&evidence.stdout_path).exists());
            assert!(std::path::Path::new(&evidence.stderr_path).exists());

            let running = status("local-preview").expect("status");
            assert!(running.running);

            let stopped = stop("local-preview").expect("stop service");
            assert!(!stopped.running);
            assert!(stopped.process.is_none());
            assert!(!std::path::Path::new(&evidence.state_path).exists());
        });
    }

    #[test]
    fn command_backend_records_public_origin_evidence_and_cleans_tunnel_process() {
        test_support::with_isolated_home(|_| {
            create_server();
            expose(ExposeServiceTunnelSpec {
                id: "public-preview".to_string(),
                server_id: "private-host".to_string(),
                target: ServiceTunnelTarget {
                    host: "127.0.0.1".to_string(),
                    port: 7331,
                },
                scheme: "http".to_string(),
                local_port: Some(8834),
                auth: ServiceTunnelAuth {
                    mode: ServiceTunnelAuthMode::BearerEnv,
                    env_var: Some("PUBLIC_PREVIEW_TOKEN".to_string()),
                    header: Some("Authorization".to_string()),
                },
                policy: ServiceTunnelPolicy {
                    exposure: ServiceTunnelExposure::PrivateLoopback,
                    require_auth: true,
                    allowed_clients: vec!["generic-browser-client".to_string()],
                },
                description: None,
            })
            .expect("expose service");

            let started = start(StartServiceTunnelSpec {
                id: "public-preview".to_string(),
                command: "while true; do sleep 1; done".to_string(),
                cwd: None,
                env: BTreeMap::new(),
                host: Some("127.0.0.1".to_string()),
                port: Some(8834),
                scheme: Some("http".to_string()),
                health_url: None,
                health_path: None,
                readiness_timeout_secs: 1,
                declared_service_host: Some("app.example.test".to_string()),
                public_tunnel_command: Some(
                    "printf 'ready https://public-preview.example.test/token?secret=1\n'; while true; do sleep 1; done"
                        .to_string(),
                ),
                public_tunnel_timeout_secs: 2,
                backend: ServiceTunnelTunnelBackend::Command,
            })
            .expect("start service with public tunnel");

            assert_eq!(
                started.public_url.as_deref(),
                Some("https://public-preview.example.test")
            );
            let backend = started.tunnel_backend.expect("backend status");
            assert_eq!(backend.backend, ServiceTunnelTunnelBackend::Command);
            assert!(backend.active);
            assert!(backend.pid.is_some());
            let origin = started.origin.expect("origin evidence");
            assert_eq!(origin.local_bind_url, "http://127.0.0.1:8834");
            assert_eq!(
                origin.declared_service_host.as_deref(),
                Some("app.example.test")
            );
            assert_eq!(
                origin.effective_browser_origin.as_deref(),
                Some("https://public-preview.example.test")
            );
            assert_eq!(origin.secure_context, Some(true));
            assert_eq!(
                origin.expected_host_header.as_deref(),
                Some("app.example.test")
            );
            assert_eq!(
                origin.public_host_matches_declared_service_host,
                Some(false)
            );
            let evidence = started.evidence.expect("evidence paths");
            assert!(std::path::Path::new(
                evidence
                    .tunnel_stdout_path
                    .as_deref()
                    .expect("tunnel stdout")
            )
            .exists());
            assert!(std::path::Path::new(
                evidence
                    .tunnel_stderr_path
                    .as_deref()
                    .expect("tunnel stderr")
            )
            .exists());

            let stopped = stop("public-preview").expect("stop service and tunnel");
            assert!(!stopped.running);
            assert!(stopped.tunnel_backend.is_none());
        });
    }

    #[test]
    fn start_cleans_runtime_state_when_readiness_fails() {
        test_support::with_isolated_home(|_| {
            create_server();
            expose(ExposeServiceTunnelSpec {
                id: "failing-preview".to_string(),
                server_id: "private-host".to_string(),
                target: ServiceTunnelTarget {
                    host: "127.0.0.1".to_string(),
                    port: 7331,
                },
                scheme: "http".to_string(),
                local_port: Some(8833),
                auth: ServiceTunnelAuth {
                    mode: ServiceTunnelAuthMode::BearerEnv,
                    env_var: Some("FAILING_PREVIEW_TOKEN".to_string()),
                    header: Some("Authorization".to_string()),
                },
                policy: ServiceTunnelPolicy {
                    exposure: ServiceTunnelExposure::PrivateLoopback,
                    require_auth: true,
                    allowed_clients: Vec::new(),
                },
                description: None,
            })
            .expect("expose service");

            let err = start(StartServiceTunnelSpec {
                id: "failing-preview".to_string(),
                command: "while true; do sleep 1; done".to_string(),
                cwd: None,
                env: BTreeMap::new(),
                host: Some("127.0.0.1".to_string()),
                port: Some(8833),
                scheme: Some("http".to_string()),
                health_url: Some("http://127.0.0.1:9/health".to_string()),
                health_path: None,
                readiness_timeout_secs: 0,
                declared_service_host: None,
                public_tunnel_command: None,
                public_tunnel_timeout_secs: 1,
                backend: ServiceTunnelTunnelBackend::None,
            })
            .expect_err("readiness should fail");

            assert_eq!(err.code, crate::core::ErrorCode::ValidationInvalidArgument);
            let state_path =
                paths::service_tunnel_runtime_state_file("failing-preview").expect("state path");
            assert!(!state_path.exists());
            let stopped = status("failing-preview").expect("status");
            assert!(!stopped.running);
        });
    }
}
