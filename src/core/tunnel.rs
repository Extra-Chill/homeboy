use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
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
    #[serde(skip_deserializing, default)]
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
    #[serde(default)]
    pub preview: ServiceTunnelPreviewPolicy,
    #[serde(
        default,
        skip_serializing_if = "ServiceTunnelNativePreviewAuthPolicy::is_default"
    )]
    pub native_preview_auth: ServiceTunnelNativePreviewAuthPolicy,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServiceTunnelNativePreviewAuthPolicy {
    #[serde(default = "default_true")]
    pub require_client_token: bool,
    #[serde(default = "default_preview_session_ttl_secs")]
    pub default_session_ttl_secs: u64,
    #[serde(default = "default_preview_session_max_ttl_secs")]
    pub max_session_ttl_secs: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_public_hosts: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_session_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tokens: Vec<ServiceTunnelNativePreviewToken>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServiceTunnelNativePreviewToken {
    pub id: String,
    pub token_sha256: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_clients: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_public_hosts: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_session_ids: Vec<String>,
    #[serde(default)]
    pub revoked: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceTunnelNativePreviewClaimRequest {
    pub client_id: String,
    pub token: String,
    pub public_host: String,
    pub session_id: String,
    pub local_origin: String,
    pub requested_ttl_secs: Option<u64>,
    pub now: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ServiceTunnelNativePreviewClaim {
    pub service_id: String,
    pub client_id: String,
    pub token_id: String,
    pub public_host: String,
    pub session_id: String,
    pub local_origin: String,
    pub expires_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServiceTunnelPreviewPolicy {
    #[serde(default)]
    pub mode: ServiceTunnelPreviewPolicyMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keep_alive_until: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ServiceTunnelPreviewPolicyMode {
    None,
    Always,
    OnFailure,
    ManualApproval,
    KeepAliveUntil,
}

impl Default for ServiceTunnelPreviewPolicy {
    fn default() -> Self {
        Self {
            mode: ServiceTunnelPreviewPolicyMode::None,
            keep_alive_until: None,
        }
    }
}

impl Default for ServiceTunnelPreviewPolicyMode {
    fn default() -> Self {
        Self::None
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ServiceTunnelStatus {
    #[serde(flatten)]
    pub preview_identity: ServiceTunnelPreviewIdentity,
    pub declared: bool,
    pub running: bool,
    pub lifecycle: String,
    pub local_url: String,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview: Option<ServiceTunnelPreviewArtifact>,
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
pub struct ServiceTunnelProcessDescriptor {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub process_group_id: Option<i32>,
    pub command: ServiceTunnelCommandSpec,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServiceTunnelLogPaths {
    pub stdout_path: String,
    pub stderr_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServiceTunnelRuntimeState {
    #[serde(flatten)]
    pub preview_identity: ServiceTunnelPreviewIdentity,
    pub pid: u32,
    #[serde(flatten)]
    pub process: ServiceTunnelProcessDescriptor,
    pub started_at: String,
    pub local_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub health_url: Option<String>,
    #[serde(flatten)]
    pub logs: ServiceTunnelLogPaths,
    pub backend: ServiceTunnelTunnelBackend,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backend_process: Option<ServiceTunnelBackendProcessState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_workflow_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ServiceTunnelTunnelBackend {
    None,
    Command,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServiceTunnelBackendProcessState {
    pub pid: u32,
    #[serde(flatten)]
    pub process: ServiceTunnelProcessDescriptor,
    pub started_at: String,
    #[serde(flatten)]
    pub logs: ServiceTunnelLogPaths,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ServiceTunnelProcessStatus {
    pub pid: u32,
    #[serde(flatten)]
    pub process: ServiceTunnelProcessDescriptor,
    pub running: bool,
    pub started_at: String,
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
    #[serde(flatten)]
    pub logs: ServiceTunnelLogPaths,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ServiceTunnelBackendStatus {
    pub backend: ServiceTunnelTunnelBackend,
    pub active: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub process: Option<ServiceTunnelProcessStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence: Option<ServiceTunnelBackendEvidence>,
}

pub type ServiceTunnelBackendEvidence = ServiceTunnelLogPaths;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServiceTunnelPreviewArtifact {
    pub schema: String,
    pub kind: String,
    #[serde(flatten)]
    pub preview_identity: ServiceTunnelPreviewIdentity,
    pub local_url: String,
    pub backend: ServiceTunnelTunnelBackend,
    pub policy: ServiceTunnelPreviewPolicy,
    pub cleanup: ServiceTunnelPreviewCleanupMetadata,
    pub source: ServiceTunnelPreviewSource,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServiceTunnelPreviewIdentity {
    pub service_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub public_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServiceTunnelPreviewCleanupMetadata {
    pub cleanup_policy: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    pub stop_on_cleanup: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServiceTunnelPreviewSource {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workflow_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceTunnelPreviewDecisionContext {
    pub run_failed: bool,
    pub manual_approval_required: bool,
    pub now: chrono::DateTime<chrono::Utc>,
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
    pub backend_command: Option<String>,
    pub backend_public_url: Option<String>,
    pub source_run_id: Option<String>,
    pub source_workflow_id: Option<String>,
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

fn default_preview_session_ttl_secs() -> u64 {
    15 * 60
}

fn default_preview_session_max_ttl_secs() -> u64 {
    60 * 60
}

impl Default for ServiceTunnelNativePreviewAuthPolicy {
    fn default() -> Self {
        Self {
            require_client_token: true,
            default_session_ttl_secs: default_preview_session_ttl_secs(),
            max_session_ttl_secs: default_preview_session_max_ttl_secs(),
            allowed_public_hosts: Vec::new(),
            allowed_session_ids: Vec::new(),
            tokens: Vec::new(),
        }
    }
}

impl ServiceTunnelNativePreviewAuthPolicy {
    fn is_default(&self) -> bool {
        self == &Self::default()
    }
}

pub fn native_preview_token_sha256(token: &str) -> String {
    let digest = Sha256::digest(token.as_bytes());
    format!("{digest:x}")
}

pub fn native_preview_token_record(
    id: impl Into<String>,
    token: &str,
) -> ServiceTunnelNativePreviewToken {
    ServiceTunnelNativePreviewToken {
        id: id.into(),
        token_sha256: native_preview_token_sha256(token),
        allowed_clients: Vec::new(),
        allowed_public_hosts: Vec::new(),
        allowed_session_ids: Vec::new(),
        revoked: false,
        expires_at: None,
    }
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
        preview_identity: ServiceTunnelPreviewIdentity {
            service_id: tunnel.id.clone(),
            public_url: spec.backend_public_url.clone(),
        },
        pid,
        process: ServiceTunnelProcessDescriptor {
            process_group_id,
            command: ServiceTunnelCommandSpec {
                command: spec.command,
                cwd: spec.cwd.map(|path| path.display().to_string()),
                env_keys: spec.env.keys().cloned().collect(),
            },
        },
        started_at: chrono::Utc::now().to_rfc3339(),
        local_url: local_url_for(&tunnel),
        health_url,
        logs: ServiceTunnelLogPaths {
            stdout_path: stdout_path.display().to_string(),
            stderr_path: stderr_path.display().to_string(),
        },
        backend: spec.backend,
        backend_process: None,
        source_run_id: spec.source_run_id,
        source_workflow_id: spec.source_workflow_id,
        readiness_kind: spec.readiness_kind,
        readiness_checks: spec.readiness_checks,
    };
    save_runtime_state(&state)?;
    if let Err(error) = wait_until_ready(&state, spec.readiness_timeout_secs) {
        terminate_runtime_state(&state)?;
        remove_runtime_state(&state.preview_identity.service_id)?;
        return Err(error);
    }
    let state = match start_backend_if_needed(state, &tunnel, spec.backend_command) {
        Ok(state) => state,
        Err(error) => {
            if let Some(state) = load_runtime_state(&tunnel.id)? {
                terminate_runtime_state(&state)?;
                remove_runtime_state(&state.preview_identity.service_id)?;
            }
            return Err(error);
        }
    };
    save_runtime_state(&state)?;
    status(&tunnel.id)
}

pub fn stop(id: &str) -> Result<ServiceTunnelStatus> {
    let tunnel = load(id)?;
    if let Some(state) = load_runtime_state(id)? {
        terminate_backend_state(&state)?;
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
    let readiness = state.as_ref().map(check_runtime_readiness);
    let evidence = state.as_ref().map(runtime_evidence);
    let process = state.as_ref().map(|state| ServiceTunnelProcessStatus {
        pid: state.pid,
        process: state.process.clone(),
        running,
        started_at: state.started_at.clone(),
    });
    let backend = state.as_ref().map(|state| ServiceTunnelBackendStatus {
        backend: state.backend.clone(),
        active: backend_state_is_running(state) || state.preview_identity.public_url.is_some(),
        process: state.backend_process.as_ref().map(|backend| {
            let running = backend_process_is_running(backend);
            ServiceTunnelProcessStatus {
                pid: backend.pid,
                process: backend.process.clone(),
                running,
                started_at: backend.started_at.clone(),
            }
        }),
        evidence: state
            .backend_process
            .as_ref()
            .map(|backend| backend.logs.clone()),
    });
    let public_url = state
        .as_ref()
        .and_then(|state| state.preview_identity.public_url.clone());
    let preview = state
        .as_ref()
        .and_then(|state| preview_artifact_for_status(tunnel, state));
    ServiceTunnelStatus {
        preview_identity: ServiceTunnelPreviewIdentity {
            service_id: tunnel.id.clone(),
            public_url,
        },
        declared: true,
        running,
        lifecycle: if running { "running" } else { "declared" }.to_string(),
        local_url: local_url_for(tunnel),
        remote_target: format!("{}:{}", tunnel.target.host, tunnel.target.port),
        policy: tunnel.policy.clone(),
        process,
        health,
        readiness,
        evidence,
        tunnel_backend: backend,
        preview,
    }
}

pub(super) fn preview_policy_allows(
    policy: &ServiceTunnelPreviewPolicy,
    context: &ServiceTunnelPreviewDecisionContext,
) -> bool {
    match policy.mode {
        ServiceTunnelPreviewPolicyMode::None => false,
        ServiceTunnelPreviewPolicyMode::Always => true,
        ServiceTunnelPreviewPolicyMode::OnFailure => context.run_failed,
        ServiceTunnelPreviewPolicyMode::ManualApproval => context.manual_approval_required,
        ServiceTunnelPreviewPolicyMode::KeepAliveUntil => policy
            .keep_alive_until
            .as_deref()
            .and_then(parse_rfc3339_utc)
            .is_some_and(|expires_at| context.now <= expires_at),
    }
}

pub(super) fn preview_artifact_for(
    tunnel: &ServiceTunnel,
    state: &ServiceTunnelRuntimeState,
    context: &ServiceTunnelPreviewDecisionContext,
) -> Option<ServiceTunnelPreviewArtifact> {
    if !preview_policy_allows(&tunnel.policy.preview, context) {
        return None;
    }

    Some(ServiceTunnelPreviewArtifact {
        schema: "homeboy/preview-url/v1".to_string(),
        kind: "preview_url".to_string(),
        preview_identity: ServiceTunnelPreviewIdentity {
            service_id: tunnel.id.clone(),
            public_url: state.preview_identity.public_url.clone(),
        },
        local_url: state.local_url.clone(),
        backend: state.backend.clone(),
        policy: tunnel.policy.preview.clone(),
        cleanup: preview_cleanup_metadata(&tunnel.policy.preview),
        source: ServiceTunnelPreviewSource {
            run_id: state.source_run_id.clone(),
            workflow_id: state.source_workflow_id.clone(),
        },
    })
}

pub fn validate_native_preview_claim(
    tunnel: &ServiceTunnel,
    request: ServiceTunnelNativePreviewClaimRequest,
) -> Result<ServiceTunnelNativePreviewClaim> {
    let policy = &tunnel.policy.native_preview_auth;
    if !policy.require_client_token {
        return Err(preview_auth_error(
            "auth",
            "native preview ingress requires client token authentication",
            Some(tunnel.id.clone()),
            Some(vec!["set require_client_token=true".to_string()]),
        ));
    }
    if request.client_id.trim().is_empty() {
        return Err(preview_auth_error(
            "client_id",
            "preview client id is required",
            Some(tunnel.id.clone()),
            None,
        ));
    }
    if request.token.trim().is_empty() {
        return Err(preview_auth_error(
            "token",
            "preview client token is required",
            Some(tunnel.id.clone()),
            None,
        ));
    }
    if request.public_host.trim().is_empty() {
        return Err(preview_auth_error(
            "public_host",
            "preview public host claim is required",
            Some(tunnel.id.clone()),
            None,
        ));
    }
    if request.session_id.trim().is_empty() {
        return Err(preview_auth_error(
            "session_id",
            "preview session id claim is required",
            Some(tunnel.id.clone()),
            None,
        ));
    }
    validate_native_preview_local_origin(&request.local_origin, &tunnel.id)?;

    let token_hash = native_preview_token_sha256(&request.token);
    let Some(token) = policy
        .tokens
        .iter()
        .find(|candidate| candidate.token_sha256 == token_hash)
    else {
        return Err(preview_auth_error(
            "token",
            "preview client token is not recognized",
            Some(tunnel.id.clone()),
            None,
        ));
    };

    if token.revoked {
        return Err(preview_auth_error(
            "token",
            "preview client token is revoked",
            Some(token.id.clone()),
            None,
        ));
    }
    if let Some(expires_at) = token.expires_at.as_deref().and_then(parse_rfc3339_utc) {
        if request.now > expires_at {
            return Err(preview_auth_error(
                "token",
                "preview client token is expired",
                Some(token.id.clone()),
                None,
            ));
        }
    }
    if !string_claim_allowed(&request.client_id, &token.allowed_clients) {
        return Err(preview_auth_error(
            "client_id",
            "preview client is not authorized for this token",
            Some(request.client_id),
            Some(token.allowed_clients.clone()),
        ));
    }
    if !host_claim_allowed(&request.public_host, &policy.allowed_public_hosts)
        || !host_claim_allowed(&request.public_host, &token.allowed_public_hosts)
    {
        return Err(preview_auth_error(
            "public_host",
            "preview token is not authorized to claim this public host",
            Some(request.public_host),
            policy_host_suggestions(policy, token),
        ));
    }
    if !string_claim_allowed(&request.session_id, &policy.allowed_session_ids)
        || !string_claim_allowed(&request.session_id, &token.allowed_session_ids)
    {
        return Err(preview_auth_error(
            "session_id",
            "preview token is not authorized to claim this session id",
            Some(request.session_id),
            policy_session_suggestions(policy, token),
        ));
    }

    let ttl_secs = request
        .requested_ttl_secs
        .unwrap_or(policy.default_session_ttl_secs)
        .min(policy.max_session_ttl_secs);
    let expires_at = request.now + chrono::Duration::seconds(ttl_secs as i64);

    Ok(ServiceTunnelNativePreviewClaim {
        service_id: tunnel.id.clone(),
        client_id: request.client_id,
        token_id: token.id.clone(),
        public_host: request.public_host,
        session_id: request.session_id,
        local_origin: request.local_origin,
        expires_at: expires_at.to_rfc3339(),
    })
}

fn preview_auth_error(
    field: &str,
    message: impl Into<String>,
    value: Option<String>,
    suggestions: Option<Vec<String>>,
) -> Error {
    Error::validation_invalid_argument(field, message, value, suggestions)
}

fn validate_native_preview_local_origin(local_origin: &str, id: &str) -> Result<()> {
    let Some(rest) = local_origin.strip_prefix("http://") else {
        return Err(preview_auth_error(
            "local_origin",
            "preview local origin must use http:// loopback",
            Some(id.to_string()),
            Some(vec!["http://127.0.0.1:<port>".to_string()]),
        ));
    };
    let host = rest.split(['/', ':']).next().unwrap_or_default();
    validate_loopback_host(host, id)
}

fn string_claim_allowed(value: &str, allowed: &[String]) -> bool {
    allowed.is_empty() || allowed.iter().any(|candidate| candidate == value)
}

fn host_claim_allowed(value: &str, allowed: &[String]) -> bool {
    allowed.is_empty()
        || allowed
            .iter()
            .any(|candidate| candidate == value || glob_match::glob_match(candidate, value))
}

fn policy_host_suggestions(
    policy: &ServiceTunnelNativePreviewAuthPolicy,
    token: &ServiceTunnelNativePreviewToken,
) -> Option<Vec<String>> {
    suggestions_from_scopes(&policy.allowed_public_hosts, &token.allowed_public_hosts)
}

fn policy_session_suggestions(
    policy: &ServiceTunnelNativePreviewAuthPolicy,
    token: &ServiceTunnelNativePreviewToken,
) -> Option<Vec<String>> {
    suggestions_from_scopes(&policy.allowed_session_ids, &token.allowed_session_ids)
}

fn suggestions_from_scopes(
    policy_values: &[String],
    token_values: &[String],
) -> Option<Vec<String>> {
    let mut suggestions = Vec::new();
    suggestions.extend(policy_values.iter().cloned());
    suggestions.extend(token_values.iter().cloned());
    if suggestions.is_empty() {
        None
    } else {
        suggestions.sort();
        suggestions.dedup();
        Some(suggestions)
    }
}

fn preview_artifact_for_status(
    tunnel: &ServiceTunnel,
    state: &ServiceTunnelRuntimeState,
) -> Option<ServiceTunnelPreviewArtifact> {
    preview_artifact_for(
        tunnel,
        state,
        &ServiceTunnelPreviewDecisionContext {
            run_failed: false,
            manual_approval_required: false,
            now: chrono::Utc::now(),
        },
    )
}

fn preview_cleanup_metadata(
    policy: &ServiceTunnelPreviewPolicy,
) -> ServiceTunnelPreviewCleanupMetadata {
    let cleanup_policy = match policy.mode {
        ServiceTunnelPreviewPolicyMode::None => "stop_immediately",
        ServiceTunnelPreviewPolicyMode::Always => "keep_while_running",
        ServiceTunnelPreviewPolicyMode::OnFailure => "keep_on_failure",
        ServiceTunnelPreviewPolicyMode::ManualApproval => "keep_for_manual_approval",
        ServiceTunnelPreviewPolicyMode::KeepAliveUntil => "keep_alive_until",
    };

    ServiceTunnelPreviewCleanupMetadata {
        cleanup_policy: cleanup_policy.to_string(),
        expires_at: policy.keep_alive_until.clone(),
        stop_on_cleanup: true,
    }
}

fn parse_rfc3339_utc(value: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|datetime| datetime.with_timezone(&chrono::Utc))
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
    if matches!(
        tunnel.policy.preview.mode,
        ServiceTunnelPreviewPolicyMode::KeepAliveUntil
    ) {
        let Some(expires_at) = tunnel.policy.preview.keep_alive_until.as_deref() else {
            return Err(Error::validation_invalid_argument(
                "policy.preview.keep_alive_until",
                "keep_alive_until preview policy requires an RFC3339 expiry",
                Some(tunnel.id.clone()),
                None,
            ));
        };
        if parse_rfc3339_utc(expires_at).is_none() {
            return Err(Error::validation_invalid_argument(
                "policy.preview.keep_alive_until",
                "preview expiry must be a valid RFC3339 timestamp",
                Some(tunnel.id.clone()),
                None,
            ));
        }
    }
    validate_native_preview_auth_policy(&tunnel.policy.native_preview_auth, &tunnel.id)?;
    Ok(())
}

fn validate_native_preview_auth_policy(
    policy: &ServiceTunnelNativePreviewAuthPolicy,
    id: &str,
) -> Result<()> {
    if policy.default_session_ttl_secs == 0 || policy.max_session_ttl_secs == 0 {
        return Err(Error::validation_invalid_argument(
            "policy.native_preview_auth.ttl",
            "native preview session TTLs must be greater than zero",
            Some(id.to_string()),
            None,
        ));
    }
    if policy.default_session_ttl_secs > policy.max_session_ttl_secs {
        return Err(Error::validation_invalid_argument(
            "policy.native_preview_auth.default_session_ttl_secs",
            "default native preview session TTL cannot exceed max_session_ttl_secs",
            Some(id.to_string()),
            None,
        ));
    }
    for token in &policy.tokens {
        if token.id.trim().is_empty() {
            return Err(Error::validation_invalid_argument(
                "policy.native_preview_auth.tokens.id",
                "native preview token id is required",
                Some(id.to_string()),
                None,
            ));
        }
        if token.token_sha256.len() != 64
            || !token
                .token_sha256
                .chars()
                .all(|character| character.is_ascii_hexdigit())
        {
            return Err(Error::validation_invalid_argument(
                "policy.native_preview_auth.tokens.token_sha256",
                "native preview tokens store a SHA-256 digest, not plaintext token material",
                Some(token.id.clone()),
                None,
            ));
        }
        if token
            .expires_at
            .as_deref()
            .is_some_and(|expires_at| parse_rfc3339_utc(expires_at).is_none())
        {
            return Err(Error::validation_invalid_argument(
                "policy.native_preview_auth.tokens.expires_at",
                "native preview token expiry must be a valid RFC3339 timestamp",
                Some(token.id.clone()),
                None,
            ));
        }
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

fn validate_backend_spec(spec: &StartServiceTunnelSpec) -> Result<()> {
    match spec.backend {
        ServiceTunnelTunnelBackend::None => Ok(()),
        ServiceTunnelTunnelBackend::Command => {
            require_backend_value(
                "public_tunnel_command",
                spec.backend_command.as_deref(),
                "command backend requires a backend command",
                &spec.id,
            )?;
            require_backend_value(
                "public_tunnel_public_url",
                spec.backend_public_url.as_deref(),
                "command backend requires the public URL it exposes",
                &spec.id,
            )?;
            Ok(())
        }
    }
}

fn require_backend_value(field: &str, value: Option<&str>, message: &str, id: &str) -> Result<()> {
    if value.unwrap_or_default().trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            field,
            message,
            Some(id.to_string()),
            None,
        ));
    }
    Ok(())
}

fn start_backend_if_needed(
    mut state: ServiceTunnelRuntimeState,
    tunnel: &ServiceTunnel,
    backend_command: Option<String>,
) -> Result<ServiceTunnelRuntimeState> {
    if !matches!(state.backend, ServiceTunnelTunnelBackend::Command) {
        return Ok(state);
    }

    let command_string = backend_command.unwrap_or_default();
    let runtime_dir = paths::service_tunnel_runtime_dir(&tunnel.id)?;
    let stdout_path = runtime_dir.join("backend-stdout.log");
    let stderr_path = runtime_dir.join("backend-stderr.log");
    let stdout = File::create(&stdout_path)
        .map_err(|e| Error::internal_io(e.to_string(), Some(stdout_path.display().to_string())))?;
    let stderr = File::create(&stderr_path)
        .map_err(|e| Error::internal_io(e.to_string(), Some(stderr_path.display().to_string())))?;

    let mut command = shell_command(&command_string);
    command
        .env("HOMEBOY_SERVICE_ID", &tunnel.id)
        .env("HOMEBOY_SERVICE_LOCAL_URL", &state.local_url);
    if let Some(public_url) = &state.preview_identity.public_url {
        command.env("HOMEBOY_TUNNEL_PUBLIC_URL", public_url);
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
            Some(format!("start service tunnel backend {}", tunnel.id)),
        )
    })?;
    let pid = child.id();
    state.backend_process = Some(ServiceTunnelBackendProcessState {
        pid,
        process: ServiceTunnelProcessDescriptor {
            process_group_id: process_group_id_for(pid),
            command: ServiceTunnelCommandSpec {
                command: command_string,
                cwd: None,
                env_keys: vec![
                    "HOMEBOY_SERVICE_ID".to_string(),
                    "HOMEBOY_SERVICE_LOCAL_URL".to_string(),
                    "HOMEBOY_TUNNEL_PUBLIC_URL".to_string(),
                ],
            },
        },
        started_at: chrono::Utc::now().to_rfc3339(),
        logs: ServiceTunnelLogPaths {
            stdout_path: stdout_path.display().to_string(),
            stderr_path: stderr_path.display().to_string(),
        },
    });
    Ok(state)
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
    let path = paths::service_tunnel_runtime_state_file(&state.preview_identity.service_id)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| Error::internal_io(e.to_string(), Some(parent.display().to_string())))?;
    }
    let data = serde_json::to_string_pretty(state).map_err(|e| {
        Error::internal_json(
            e.to_string(),
            Some(state.preview_identity.service_id.clone()),
        )
    })?;
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
    if let Some(pgid) = state.process.process_group_id {
        process_group_is_running(pgid)
    } else {
        pid_is_running(state.pid)
    }
}

fn backend_state_is_running(state: &ServiceTunnelRuntimeState) -> bool {
    state
        .backend_process
        .as_ref()
        .is_some_and(backend_process_is_running)
}

fn backend_process_is_running(state: &ServiceTunnelBackendProcessState) -> bool {
    if let Some(pgid) = state.process.process_group_id {
        process_group_is_running(pgid)
    } else {
        pid_is_running(state.pid)
    }
}

fn terminate_backend_state(state: &ServiceTunnelRuntimeState) -> Result<()> {
    let Some(backend) = &state.backend_process else {
        return Ok(());
    };
    if !backend_process_is_running(backend) {
        return Ok(());
    }

    #[cfg(unix)]
    unsafe {
        if let Some(pgid) = backend.process.process_group_id {
            libc::kill(-(pgid as libc::pid_t), libc::SIGTERM);
            std::thread::sleep(Duration::from_millis(250));
            if process_group_is_running(pgid) {
                libc::kill(-(pgid as libc::pid_t), libc::SIGKILL);
            }
        } else {
            libc::kill(backend.pid as libc::pid_t, libc::SIGTERM);
        }
    }

    #[cfg(not(unix))]
    {
        let _ = backend;
    }

    Ok(())
}

fn terminate_runtime_state(state: &ServiceTunnelRuntimeState) -> Result<()> {
    if !runtime_state_is_running(state) {
        return Ok(());
    }

    #[cfg(unix)]
    unsafe {
        if let Some(pgid) = state.process.process_group_id {
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
        let readiness = check_runtime_readiness(state);
        if !readiness.process_running {
            return Err(Error::validation_invalid_argument(
                "service",
                "service process exited before becoming ready",
                Some(state.preview_identity.service_id.clone()),
                None,
            ));
        }
        if readiness.ready {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(Error::validation_invalid_argument(
                "readiness",
                "service did not satisfy readiness before timeout",
                Some(state.preview_identity.service_id.clone()),
                Some(
                    readiness
                        .checks
                        .into_iter()
                        .filter(|check| !check.ready)
                        .filter_map(|check| check.detail)
                        .collect(),
                ),
            ));
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn check_runtime_readiness(state: &ServiceTunnelRuntimeState) -> ServiceTunnelReadinessStatus {
    let process_running = runtime_state_is_running(state);
    let mut checks = Vec::new();

    if state.health_url.is_some() {
        let health = check_runtime_health(state);
        checks.push(ServiceTunnelReadinessCheckStatus {
            check: "health".to_string(),
            ready: health.healthy,
            detail: health
                .status_code
                .map(|status| format!("status {status}"))
                .or(health.error),
        });
    }

    for check in &state.readiness_checks {
        checks.push(evaluate_readiness_check(state, check));
    }

    let checks_ready = checks.iter().all(|check| check.ready);
    let ready = process_running && checks_ready;
    ServiceTunnelReadinessStatus {
        kind: state.readiness_kind.clone(),
        process_running,
        ready,
        preview_ready: matches!(state.readiness_kind, ServiceTunnelReadinessKind::Preview) && ready,
        proof_ready: matches!(state.readiness_kind, ServiceTunnelReadinessKind::Proof) && ready,
        checks,
    }
}

fn evaluate_readiness_check(
    state: &ServiceTunnelRuntimeState,
    check: &ServiceTunnelReadinessCheck,
) -> ServiceTunnelReadinessCheckStatus {
    match check {
        ServiceTunnelReadinessCheck::TcpListener => tcp_listener_readiness(state),
        ServiceTunnelReadinessCheck::ArtifactJsonPointer {
            path,
            pointer,
            equals,
        } => artifact_json_pointer_readiness(path, pointer, equals),
        ServiceTunnelReadinessCheck::StdoutRegex { pattern } => {
            stdout_regex_readiness(&state.logs.stdout_path, pattern)
        }
    }
}

fn tcp_listener_readiness(state: &ServiceTunnelRuntimeState) -> ServiceTunnelReadinessCheckStatus {
    let Some((host, port)) = local_url_host_port(&state.local_url) else {
        return ServiceTunnelReadinessCheckStatus {
            check: "tcp_listener".to_string(),
            ready: false,
            detail: Some(format!("could not parse local URL {}", state.local_url)),
        };
    };
    let address = format!("{host}:{port}");
    match address.to_socket_addrs() {
        Ok(mut addresses) => {
            let ready = addresses.any(|address| {
                TcpStream::connect_timeout(&address, Duration::from_millis(200)).is_ok()
            });
            ServiceTunnelReadinessCheckStatus {
                check: "tcp_listener".to_string(),
                ready,
                detail: Some(address),
            }
        }
        Err(error) => ServiceTunnelReadinessCheckStatus {
            check: "tcp_listener".to_string(),
            ready: false,
            detail: Some(error.to_string()),
        },
    }
}

fn artifact_json_pointer_readiness(
    path: &str,
    pointer: &str,
    equals: &str,
) -> ServiceTunnelReadinessCheckStatus {
    let data = match fs::read_to_string(path) {
        Ok(data) => data,
        Err(error) => {
            return ServiceTunnelReadinessCheckStatus {
                check: "artifact_json_pointer".to_string(),
                ready: false,
                detail: Some(format!("{}: {error}", path)),
            };
        }
    };
    let json: serde_json::Value = match serde_json::from_str(&data) {
        Ok(json) => json,
        Err(error) => {
            return ServiceTunnelReadinessCheckStatus {
                check: "artifact_json_pointer".to_string(),
                ready: false,
                detail: Some(format!("{}: {error}", path)),
            };
        }
    };
    let Some(value) = json.pointer(pointer) else {
        return ServiceTunnelReadinessCheckStatus {
            check: "artifact_json_pointer".to_string(),
            ready: false,
            detail: Some(format!("{path} missing {pointer}")),
        };
    };
    let actual = value
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| value.to_string());
    ServiceTunnelReadinessCheckStatus {
        check: "artifact_json_pointer".to_string(),
        ready: actual == equals,
        detail: Some(format!("{path} {pointer}={actual}")),
    }
}

fn stdout_regex_readiness(path: &str, pattern: &str) -> ServiceTunnelReadinessCheckStatus {
    let data = match fs::read_to_string(path) {
        Ok(data) => data,
        Err(error) => {
            return ServiceTunnelReadinessCheckStatus {
                check: "stdout_regex".to_string(),
                ready: false,
                detail: Some(format!("{}: {error}", path)),
            };
        }
    };
    match regex::Regex::new(pattern) {
        Ok(regex) => ServiceTunnelReadinessCheckStatus {
            check: "stdout_regex".to_string(),
            ready: regex.is_match(&data),
            detail: Some(pattern.to_string()),
        },
        Err(error) => ServiceTunnelReadinessCheckStatus {
            check: "stdout_regex".to_string(),
            ready: false,
            detail: Some(error.to_string()),
        },
    }
}

fn local_url_host_port(local_url: &str) -> Option<(String, u16)> {
    let url = reqwest::Url::parse(local_url).ok()?;
    Some((url.host_str()?.to_string(), url.port_or_known_default()?))
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
        state_path: paths::service_tunnel_runtime_state_file(&state.preview_identity.service_id)
            .map(|path| path.display().to_string())
            .unwrap_or_default(),
        logs: state.logs.clone(),
    }
}
