use serde::{Deserialize, Serialize};

use super::declaration::{ServiceTunnelPolicy, ServiceTunnelPreviewPolicy};

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ServiceTunnelStatus {
    #[serde(flatten)]
    pub preview_identity: ServiceTunnelPreviewIdentity,
    pub declared: bool,
    pub running: bool,
    pub lifecycle: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub degraded_reason: Option<String>,
    pub local_url: String,
    pub remote_target: String,
    pub policy: ServiceTunnelPolicy,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub process: Option<ServiceTunnelProcessStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub health: Option<ServiceTunnelHealthStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub readiness: Option<ServiceTunnelReadinessStatus>,
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
    #[serde(default)]
    pub readiness_kind: ServiceTunnelReadinessKind,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub readiness_checks: Vec<ServiceTunnelReadinessCheck>,
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum ServiceTunnelReadinessKind {
    #[default]
    Process,
    Preview,
    Proof,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServiceTunnelReadinessCheck {
    TcpListener,
    ArtifactJsonPointer {
        path: String,
        pointer: String,
        equals: String,
    },
    StdoutRegex {
        pattern: String,
    },
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ServiceTunnelReadinessStatus {
    pub kind: ServiceTunnelReadinessKind,
    pub process_running: bool,
    pub ready: bool,
    pub preview_ready: bool,
    pub proof_ready: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checks: Vec<ServiceTunnelReadinessCheckStatus>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ServiceTunnelReadinessCheckStatus {
    pub check: String,
    pub ready: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
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
    pub active_reason: Option<String>,
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
