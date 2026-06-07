use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::path::PathBuf;
use std::process::{Command, Stdio};
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
    pub command: ServiceTunnelCommandSpec,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub health_url: Option<String>,
    pub stdout_path: String,
    pub stderr_path: String,
    pub backend: ServiceTunnelTunnelBackend,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backend_state: Option<ServiceTunnelBackendRuntimeState>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ServiceTunnelTunnelBackend {
    None,
    Traforo,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServiceTunnelBackendRuntimeState {
    pub backend: ServiceTunnelTunnelBackend,
    pub tunnel_id: String,
    pub public_url: String,
    pub pid: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub process_group_id: Option<i32>,
    pub started_at: String,
    pub command: String,
    pub local_host: String,
    pub local_port: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_url: Option<String>,
    pub base_domain: String,
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
    pub backend_stdout_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backend_stderr_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ServiceTunnelBackendStatus {
    pub backend: ServiceTunnelTunnelBackend,
    pub active: bool,
    pub health: ServiceTunnelBackendHealthStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tunnel_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub public_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub process_group_id: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_mapping: Option<ServiceTunnelBackendMapping>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stdout_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stderr_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ServiceTunnelBackendHealthStatus {
    pub running: bool,
    pub healthy: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ServiceTunnelBackendMapping {
    pub local_host: String,
    pub local_port: u16,
    pub local_url: String,
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
    pub backend: ServiceTunnelTunnelBackend,
    pub public_tunnel_id: Option<String>,
    pub public_tunnel_server_url: Option<String>,
    pub public_tunnel_base_domain: Option<String>,
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

    if let Some(host) = &spec.host {
        validate_loopback_host(&host, &tunnel.id)?;
        tunnel.local_host = host.clone();
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
    if let Some(scheme) = &spec.scheme {
        tunnel.scheme = scheme.clone();
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
    let health_url = resolve_health_url(&tunnel, spec.health_url.clone(), spec.health_path.clone());
    let mut state = ServiceTunnelRuntimeState {
        service_id: tunnel.id.clone(),
        pid,
        process_group_id,
        started_at: chrono::Utc::now().to_rfc3339(),
        local_url: local_url_for(&tunnel),
        public_url: None,
        command: ServiceTunnelCommandSpec {
            command: spec.command.clone(),
            cwd: spec.cwd.as_ref().map(|path| path.display().to_string()),
            env_keys: spec.env.keys().cloned().collect(),
        },
        health_url,
        stdout_path: stdout_path.display().to_string(),
        stderr_path: stderr_path.display().to_string(),
        backend: spec.backend.clone(),
        backend_state: None,
    };
    save_runtime_state(&state)?;
    if let Err(error) = wait_until_ready(&state, spec.readiness_timeout_secs) {
        terminate_runtime_state(&state)?;
        remove_runtime_state(&state.service_id)?;
        return Err(error);
    }
    if let Err(error) = start_backend(&mut state, &spec) {
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
    let backend = state.as_ref().map(service_tunnel_backend_status);
    let public_url = state.as_ref().and_then(|state| state.public_url.clone());
    ServiceTunnelStatus {
        service_id: tunnel.id.clone(),
        declared: true,
        running,
        lifecycle: if running { "running" } else { "declared" }.to_string(),
        local_url: local_url_for(tunnel),
        public_url,
        remote_target: format!("{}:{}", tunnel.target.host, tunnel.target.port),
        policy: tunnel.policy.clone(),
        process,
        health,
        evidence,
        tunnel_backend: backend,
    }
}

fn service_tunnel_backend_status(state: &ServiceTunnelRuntimeState) -> ServiceTunnelBackendStatus {
    let Some(backend_state) = &state.backend_state else {
        return ServiceTunnelBackendStatus {
            backend: state.backend.clone(),
            active: false,
            health: ServiceTunnelBackendHealthStatus {
                running: false,
                healthy: matches!(state.backend, ServiceTunnelTunnelBackend::None),
                error: None,
            },
            tunnel_id: None,
            public_url: None,
            pid: None,
            process_group_id: None,
            started_at: None,
            command: None,
            local_mapping: None,
            stdout_path: None,
            stderr_path: None,
        };
    };

    let running = runtime_backend_is_running(backend_state);
    ServiceTunnelBackendStatus {
        backend: backend_state.backend.clone(),
        active: running,
        health: ServiceTunnelBackendHealthStatus {
            running,
            healthy: running,
            error: if running {
                None
            } else {
                Some("backend process is not running".to_string())
            },
        },
        tunnel_id: Some(backend_state.tunnel_id.clone()),
        public_url: Some(backend_state.public_url.clone()),
        pid: Some(backend_state.pid),
        process_group_id: backend_state.process_group_id,
        started_at: Some(backend_state.started_at.clone()),
        command: Some(backend_state.command.clone()),
        local_mapping: Some(ServiceTunnelBackendMapping {
            local_host: backend_state.local_host.clone(),
            local_port: backend_state.local_port,
            local_url: state.local_url.clone(),
        }),
        stdout_path: Some(backend_state.stdout_path.clone()),
        stderr_path: Some(backend_state.stderr_path.clone()),
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
    if let Some(backend_state) = &state.backend_state {
        terminate_process(backend_state.pid, backend_state.process_group_id);
    }

    if !runtime_state_is_running(state) {
        return Ok(());
    }

    terminate_process(state.pid, state.process_group_id);
    Ok(())
}

fn terminate_process(pid: u32, process_group_id: Option<i32>) {
    #[cfg(unix)]
    unsafe {
        if let Some(pgid) = process_group_id {
            libc::kill(-(pgid as libc::pid_t), libc::SIGTERM);
            std::thread::sleep(Duration::from_millis(250));
            if process_group_is_running(pgid) {
                libc::kill(-(pgid as libc::pid_t), libc::SIGKILL);
            }
        } else {
            libc::kill(pid as libc::pid_t, libc::SIGTERM);
        }
    }

    #[cfg(not(unix))]
    {
        let _ = pid;
        let _ = process_group_id;
    }
}

fn runtime_backend_is_running(state: &ServiceTunnelBackendRuntimeState) -> bool {
    if let Some(pgid) = state.process_group_id {
        process_group_is_running(pgid)
    } else {
        pid_is_running(state.pid)
    }
}

fn start_backend(
    state: &mut ServiceTunnelRuntimeState,
    spec: &StartServiceTunnelSpec,
) -> Result<()> {
    match state.backend {
        ServiceTunnelTunnelBackend::None => Ok(()),
        ServiceTunnelTunnelBackend::Traforo => start_traforo_backend(state, spec),
    }
}

fn start_traforo_backend(
    state: &mut ServiceTunnelRuntimeState,
    spec: &StartServiceTunnelSpec,
) -> Result<()> {
    let Some(local_port) = state
        .local_url
        .rsplit_once(':')
        .and_then(|(_, port)| port.parse::<u16>().ok())
    else {
        return Err(Error::validation_invalid_argument(
            "port",
            "traforo backend requires an explicit local port",
            Some(state.service_id.clone()),
            None,
        ));
    };

    let runtime_dir = paths::service_tunnel_runtime_dir(&state.service_id)?;
    let stdout_path = runtime_dir.join("backend-traforo.stdout.log");
    let stderr_path = runtime_dir.join("backend-traforo.stderr.log");
    let stdout = File::create(&stdout_path)
        .map_err(|e| Error::internal_io(e.to_string(), Some(stdout_path.display().to_string())))?;
    let stderr = File::create(&stderr_path)
        .map_err(|e| Error::internal_io(e.to_string(), Some(stderr_path.display().to_string())))?;

    let tunnel_id = spec
        .public_tunnel_id
        .clone()
        .unwrap_or_else(generate_public_tunnel_id);
    validate_public_tunnel_id(&tunnel_id, &state.service_id)?;
    let base_domain = spec
        .public_tunnel_base_domain
        .clone()
        .or_else(|| std::env::var("TRAFORO_BASE_DOMAIN").ok())
        .filter(|domain| !domain.trim().is_empty())
        .unwrap_or_else(|| "traforo.dev".to_string());
    let public_url = format!("https://{}-tunnel.{}", tunnel_id, base_domain);
    let traforo_bin = std::env::var("HOMEBOY_TRAFORO_BIN")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "traforo".to_string());

    let mut args = vec![
        "-p".to_string(),
        local_port.to_string(),
        "-h".to_string(),
        local_host_from_url(&state.local_url),
        "-t".to_string(),
        tunnel_id.clone(),
    ];
    if let Some(server_url) = spec.public_tunnel_server_url.as_ref() {
        args.push("-s".to_string());
        args.push(server_url.clone());
    }

    let mut command = Command::new(&traforo_bin);
    command
        .args(&args)
        .env("TRAFORO_BASE_DOMAIN", &base_domain)
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
            Some(format!("start traforo backend for {}", state.service_id)),
        )
    })?;
    let pid = child.id();
    let process_group_id = process_group_id_for(pid);
    let backend_state = ServiceTunnelBackendRuntimeState {
        backend: ServiceTunnelTunnelBackend::Traforo,
        tunnel_id,
        public_url: public_url.clone(),
        pid,
        process_group_id,
        started_at: chrono::Utc::now().to_rfc3339(),
        command: format!("{} {}", traforo_bin, shell_words(&args)),
        local_host: local_host_from_url(&state.local_url),
        local_port,
        server_url: spec.public_tunnel_server_url.clone(),
        base_domain,
        stdout_path: stdout_path.display().to_string(),
        stderr_path: stderr_path.display().to_string(),
    };
    std::thread::sleep(Duration::from_millis(250));
    if !runtime_backend_is_running(&backend_state) {
        let stderr_snippet = read_log_tail(&backend_state.stderr_path, 2048).unwrap_or_default();
        return Err(Error::validation_invalid_argument(
            "public_tunnel_backend",
            "traforo backend exited before becoming ready",
            Some(state.service_id.clone()),
            if stderr_snippet.trim().is_empty() {
                None
            } else {
                Some(vec![stderr_snippet])
            },
        ));
    }

    state.public_url = Some(public_url);
    state.backend_state = Some(backend_state);
    Ok(())
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
        backend_stdout_path: state
            .backend_state
            .as_ref()
            .map(|backend| backend.stdout_path.clone()),
        backend_stderr_path: state
            .backend_state
            .as_ref()
            .map(|backend| backend.stderr_path.clone()),
    }
}

fn generate_public_tunnel_id() -> String {
    format!("hb{}", uuid::Uuid::new_v4().simple())
}

fn validate_public_tunnel_id(id: &str, service_id: &str) -> Result<()> {
    if id.len() < 12
        || !id
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
    {
        return Err(Error::validation_invalid_argument(
            "public_tunnel_id",
            "public tunnel IDs must be at least 12 chars and contain only lowercase letters, digits, or hyphens",
            Some(service_id.to_string()),
            None,
        ));
    }
    Ok(())
}

fn local_host_from_url(local_url: &str) -> String {
    local_url
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(local_url)
        .rsplit_once(':')
        .map(|(host, _)| host.to_string())
        .unwrap_or_else(|| "127.0.0.1".to_string())
}

fn shell_words(args: &[String]) -> String {
    args.iter()
        .map(|arg| {
            if arg.chars().all(|ch| {
                ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-' | '/' | ':' | '=')
            }) {
                arg.clone()
            } else {
                format!("'{}'", arg.replace('\'', "'\\''"))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn read_log_tail(path: &str, max_bytes: usize) -> Option<String> {
    let data = fs::read(path).ok()?;
    let start = data.len().saturating_sub(max_bytes);
    Some(String::from_utf8_lossy(&data[start..]).to_string())
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
                id: "context-a8c".to_string(),
                server_id: "private-host".to_string(),
                target: ServiceTunnelTarget {
                    host: "127.0.0.1".to_string(),
                    port: 7331,
                },
                scheme: "http".to_string(),
                local_port: Some(8831),
                auth: ServiceTunnelAuth {
                    mode: ServiceTunnelAuthMode::BearerEnv,
                    env_var: Some("CONTEXTA8C_TOKEN".to_string()),
                    header: Some("Authorization".to_string()),
                },
                policy: ServiceTunnelPolicy {
                    exposure: ServiceTunnelExposure::PrivateLoopback,
                    require_auth: true,
                    allowed_clients: vec!["wp-runtime".to_string()],
                },
                description: Some("Private MCP service".to_string()),
            })
            .expect("expose service");

            assert_eq!(tunnel.id, "context-a8c");
            let report = status("context-a8c").expect("status");
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
                    allowed_clients: vec!["wpcom-calypso".to_string()],
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
                backend: ServiceTunnelTunnelBackend::None,
                public_tunnel_id: None,
                public_tunnel_server_url: None,
                public_tunnel_base_domain: None,
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
    fn traforo_backend_records_public_url_process_metadata_and_cleans_up() {
        test_support::with_isolated_home(|home| {
            create_server();
            let fake_traforo = home.path().join("fake-traforo.sh");
            fs::write(
                &fake_traforo,
                "#!/usr/bin/env sh\necho \"fake traforo $@\"\nwhile true; do sleep 1; done\n",
            )
            .expect("fake traforo");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mut permissions = fs::metadata(&fake_traforo)
                    .expect("fake traforo metadata")
                    .permissions();
                permissions.set_mode(0o755);
                fs::set_permissions(&fake_traforo, permissions).expect("fake traforo executable");
            }
            let prior_traforo_bin = std::env::var("HOMEBOY_TRAFORO_BIN").ok();
            std::env::set_var("HOMEBOY_TRAFORO_BIN", &fake_traforo);

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
                    allowed_clients: Vec::new(),
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
                backend: ServiceTunnelTunnelBackend::Traforo,
                public_tunnel_id: Some("custompublic123".to_string()),
                public_tunnel_server_url: Some("wss://relay.example.test".to_string()),
                public_tunnel_base_domain: Some("example.test".to_string()),
            })
            .expect("start public service");

            assert_eq!(
                started.public_url.as_deref(),
                Some("https://custompublic123-tunnel.example.test")
            );
            let backend = started.tunnel_backend.expect("backend status");
            assert_eq!(backend.backend, ServiceTunnelTunnelBackend::Traforo);
            assert!(backend.active);
            assert!(backend.health.healthy);
            assert_eq!(backend.tunnel_id.as_deref(), Some("custompublic123"));
            assert_eq!(backend.local_mapping.expect("mapping").local_port, 8834);
            let backend_stdout = backend.stdout_path.expect("backend stdout");
            let backend_stderr = backend.stderr_path.expect("backend stderr");
            assert!(std::path::Path::new(&backend_stdout).exists());
            assert!(std::path::Path::new(&backend_stderr).exists());
            let evidence = started.evidence.expect("evidence");
            assert_eq!(
                evidence.backend_stdout_path.as_deref(),
                Some(backend_stdout.as_str())
            );
            assert_eq!(
                evidence.backend_stderr_path.as_deref(),
                Some(backend_stderr.as_str())
            );

            let stopped = stop("public-preview").expect("stop public service");
            assert!(!stopped.running);
            assert!(stopped.tunnel_backend.is_none());

            match prior_traforo_bin {
                Some(value) => std::env::set_var("HOMEBOY_TRAFORO_BIN", value),
                None => std::env::remove_var("HOMEBOY_TRAFORO_BIN"),
            }
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
                backend: ServiceTunnelTunnelBackend::None,
                public_tunnel_id: None,
                public_tunnel_server_url: None,
                public_tunnel_base_domain: None,
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
