use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::sync::{Condvar, Mutex};

use crate::core::daemon::ServiceIdentity;
use crate::core::plan::HomeboyPlan;
use crate::core::preview_client::{
    PreviewIngressRequest, PreviewIngressResponse, PreviewIngressResponseChunk,
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PreviewIngressRoute {
    pub session_id: String,
    pub public_host: String,
    pub upstream_origin: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    #[serde(default = "default_true")]
    pub active: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PreviewIngressStatus {
    pub bind: Option<String>,
    pub domain: Option<String>,
    pub public_host_pattern: Option<String>,
    pub routes: Vec<PreviewIngressRouteStatus>,
    pub recent_failures: Vec<PreviewIngressFailure>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inspected_host: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inspected_state: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PreviewIngressRouteStatus {
    #[serde(flatten)]
    pub route: PreviewIngressRoute,
    pub lifecycle: PreviewIngressRouteLifecycle,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PreviewIngressRouteLifecycle {
    Active,
    Expired,
    Disconnected,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PreviewIngressFailure {
    pub request_id: String,
    pub host: String,
    pub path: String,
    pub status: u16,
    pub classification: String,
    pub message: String,
}

#[derive(Debug, Clone)]
pub struct PreviewIngressServeSpec {
    pub bind: String,
    pub domain: String,
    pub public_host_pattern: String,
    pub token_sha256_env: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewIngressInstallOptions {
    pub server_id: String,
    pub domain: String,
    pub public_host_pattern: String,
    pub bind: String,
    pub binary_path: String,
    pub service_name: String,
    pub identity: ServiceIdentity,
}

impl Default for PreviewIngressInstallOptions {
    fn default() -> Self {
        Self {
            server_id: String::new(),
            domain: String::new(),
            public_host_pattern: String::new(),
            bind: "127.0.0.1:7350".to_string(),
            binary_path: "/usr/local/bin/homeboy".to_string(),
            service_name: "homeboy-preview-ingress".to_string(),
            identity: ServiceIdentity::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct PreviewIngressInstallPlan {
    pub command: String,
    pub plan: HomeboyPlan,
    pub server_id: String,
    pub domain: String,
    pub public_host_pattern: String,
    pub dns_probe_host: String,
    pub bind: String,
    pub service_name: String,
    #[serde(flatten)]
    pub identity: ServiceIdentity,
    pub binary_path: String,
    pub local_status_url: String,
    pub public_status_url: String,
    pub dry_run: bool,
    pub applied: bool,
    pub writes: Vec<PreviewIngressWrite>,
    pub systemd_unit: String,
    pub nginx_site: String,
    pub caddy_site: String,
    pub install_commands: Vec<String>,
    pub status_commands: Vec<String>,
    pub rollback_commands: Vec<String>,
    pub smoke_checks: Vec<String>,
    pub required_operator_config: Vec<String>,
    pub secrets_policy: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PreviewIngressWrite {
    pub path: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct PreviewIngressInstallStatusPlan {
    pub command: String,
    pub plan: HomeboyPlan,
    pub server_id: String,
    pub domain: String,
    pub public_host_pattern: String,
    pub dns_probe_host: String,
    pub bind: String,
    pub service_name: String,
    pub local_status_url: String,
    pub public_status_url: String,
    pub probed: bool,
    pub checks: Vec<PreviewIngressInstallCheck>,
    pub status_commands: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PreviewIngressInstallCheck {
    pub name: String,
    pub command: String,
    pub status: PreviewIngressInstallCheckStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stdout: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stderr: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PreviewIngressInstallCheckStatus {
    Planned,
    Passed,
    Failed,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct PreviewIngressLogLine {
    pub(crate) request_id: String,
    pub(crate) host: String,
    pub(crate) path: String,
    pub(crate) status: u16,
    pub(crate) bytes: usize,
    pub(crate) duration_ms: u128,
    pub(crate) classification: String,
}

#[derive(Debug, Clone)]
pub(crate) struct PreviewClientSession {
    pub(crate) local_origin: String,
    pub(crate) pending: VecDeque<PreviewIngressRequest>,
    pub(crate) responses: HashMap<String, PreviewIngressResponse>,
    pub(crate) response_chunks: HashMap<String, VecDeque<PreviewIngressResponseChunk>>,
    pub(crate) active: bool,
}

#[derive(Debug, Default)]
pub(crate) struct PreviewClientSessions {
    pub(crate) sessions: Mutex<HashMap<String, PreviewClientSession>>,
    pub(crate) changed: Condvar,
}

#[derive(Debug, Clone)]
pub(crate) struct PreviewIngressAuth {
    pub(crate) token_sha256_env: String,
    pub(crate) token_sha256: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct PreviewRegisterRequest {
    pub(crate) public_host: String,
    pub(crate) local_origin: String,
    #[serde(default)]
    pub(crate) session_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct PreviewNextRequest {
    pub(crate) public_host: String,
    #[serde(default)]
    pub(crate) timeout_secs: u64,
}

#[derive(Debug, Deserialize)]
pub(crate) struct PreviewRespondRequest {
    pub(crate) public_host: String,
    pub(crate) response: PreviewIngressResponse,
}

#[derive(Debug, Deserialize)]
pub(crate) struct PreviewRespondChunkRequest {
    pub(crate) public_host: String,
    pub(crate) chunk: PreviewIngressResponseChunk,
}

#[derive(Debug, Deserialize)]
pub(crate) struct PreviewCloseRequest {
    pub(crate) public_host: String,
}

fn default_true() -> bool {
    true
}
