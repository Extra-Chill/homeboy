use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

use super::runtime_state::{
    ServiceTunnelReadinessCheck, ServiceTunnelReadinessKind, ServiceTunnelTunnelBackend,
};

/// Sentinel server id used for runner-local service tunnels. A service tunnel
/// carrying this server id is materialized/validated without requiring a
/// separate `server` declaration: in a runner-local context the runner itself
/// is the server, so demanding a duplicate server declaration is redundant
/// (see #4606).
pub const RUNNER_LOCAL_SERVICE_SERVER_ID: &str = "__runner_local__";

/// Returns true when the given server id refers to the runner-local sentinel.
pub fn is_runner_local_server_id(server_id: &str) -> bool {
    server_id == RUNNER_LOCAL_SERVICE_SERVER_ID
}

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
#[derive(Default)]
pub enum ServiceTunnelPreviewPolicyMode {
    #[default]
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
    pub readiness_kind: ServiceTunnelReadinessKind,
    pub readiness_checks: Vec<ServiceTunnelReadinessCheck>,
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
    /// When true, the declaration is materialized against the runner-local
    /// sentinel server instead of requiring a separate `server` declaration for
    /// the selected runner (see #4606). The supplied `server_id` is ignored.
    pub runner_local: bool,
}

pub(in crate::core::tunnel) fn default_scheme() -> String {
    "http".to_string()
}

pub(in crate::core::tunnel) fn default_local_host() -> String {
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
