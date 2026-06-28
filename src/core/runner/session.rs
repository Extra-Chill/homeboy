use serde::{Deserialize, Serialize};

use crate::core::redaction::redact_argv_display;

use crate::core::api_jobs::{ActiveRunnerJobSummary, Job, JobArtifactMetadata, JobStatus};

mod session_enums {
    use super::*;

    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
    #[serde(rename_all = "snake_case")]
    pub enum RunnerLifecycleOwner {
        Controller,
        Runner,
        Broker,
        Local,
    }

    impl RunnerLifecycleOwner {
        pub fn as_str(&self) -> &'static str {
            match self {
                Self::Controller => "controller",
                Self::Runner => "runner",
                Self::Broker => "broker",
                Self::Local => "local",
            }
        }
    }

    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
    #[serde(rename_all = "snake_case")]
    pub enum RunnerTunnelMode {
        DirectSsh,
        Reverse,
    }

    impl RunnerTunnelMode {
        pub fn label(&self) -> &'static str {
            self.labels().0
        }

        pub fn metadata_value(&self) -> &'static str {
            self.labels().1
        }

        fn labels(&self) -> (&'static str, &'static str) {
            match self {
                RunnerTunnelMode::DirectSsh => ("direct SSH", "direct_ssh"),
                RunnerTunnelMode::Reverse => ("reverse-connected", "reverse"),
            }
        }
    }

    pub(super) fn default_tunnel_mode() -> RunnerTunnelMode {
        RunnerTunnelMode::DirectSsh
    }

    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
    #[serde(rename_all = "snake_case")]
    pub enum RunnerSessionRole {
        Controller,
        Runner,
    }

    pub(super) fn default_session_role() -> RunnerSessionRole {
        RunnerSessionRole::Controller
    }

    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
    #[serde(rename_all = "snake_case")]
    pub enum RunnerSessionState {
        Connected,
        Disconnected,
        Recorded,
    }

    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
    #[serde(rename_all = "snake_case")]
    pub enum RunnerActiveJobState {
        Available,
        Unavailable,
        NotQueried,
    }

    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
    #[serde(rename_all = "snake_case")]
    pub enum RunnerActiveJobSource {
        DirectDaemon,
        ReverseBroker,
    }
}
pub use session_enums::*;
use session_enums::{default_session_role, default_tunnel_mode};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunnerAvailability {
    pub runner_id: String,
    pub connected: bool,
    pub accepts_jobs: bool,
    pub active_job_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capacity: Option<usize>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reasons: Vec<String>,
}

impl RunnerAvailability {
    pub fn from_status_parts(
        runner_id: impl Into<String>,
        connected: bool,
        stale_daemon: bool,
        active_job_count: usize,
        active_job_state: &RunnerActiveJobState,
        capacity: Option<usize>,
    ) -> Self {
        let mut reasons = Vec::new();
        if !connected {
            reasons.push("not_connected".to_string());
        }
        if stale_daemon {
            reasons.push("stale_daemon".to_string());
        }
        match capacity {
            Some(capacity) if active_job_count >= capacity => {
                reasons.push("capacity_reached".to_string());
            }
            None if active_job_count > 0 => {
                reasons.push("capacity_unknown".to_string());
            }
            _ => {}
        }
        // `active_job_state` reflects a single live poll of the runner daemon's
        // `/jobs` endpoint (see `connection::status`): `Available` means the
        // poll succeeded, `Unavailable` means it failed, `NotQueried` means it
        // was skipped. That poll is a soft, transient signal — a connected,
        // under-capacity runner whose poll briefly failed (request timeout, or
        // a race with a daemon refresh) is reported `Available` again on the
        // very next poll, which is exactly why `runner status` and lab-offload
        // preflight could disagree about the same runner. direct_daemon /
        // direct_ssh runners also legitimately have no worker process to
        // consult and answer `/jobs` straight from the daemon job store.
        //
        // So a failed/absent active-job poll is only treated as a blocking
        // reason when the runner is already unusable for a hard reason
        // (disconnected, stale daemon, or at capacity). For a connected,
        // under-capacity runner it is intentionally non-blocking, keeping
        // `accepts_jobs` aligned with the `active_job_state: available` verdict
        // the status path derives from the daemon's own report.
        if *active_job_state != RunnerActiveJobState::Available && !reasons.is_empty() {
            reasons.push(
                match active_job_state {
                    RunnerActiveJobState::Unavailable => "active_jobs_unavailable",
                    RunnerActiveJobState::NotQueried => "active_jobs_not_queried",
                    RunnerActiveJobState::Available => unreachable!(),
                }
                .to_string(),
            );
        }
        let accepts_jobs = reasons.is_empty();
        Self {
            runner_id: runner_id.into(),
            connected,
            accepts_jobs,
            active_job_count,
            capacity,
            reasons,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunnerActiveJobError {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunnerSession {
    pub runner_id: String,
    #[serde(default = "default_tunnel_mode")]
    pub mode: RunnerTunnelMode,
    #[serde(default = "default_session_role")]
    pub role: RunnerSessionRole,
    pub server_id: Option<String>,
    #[serde(default)]
    pub controller_id: Option<String>,
    #[serde(default)]
    pub broker_url: Option<String>,
    #[serde(default)]
    pub remote_daemon_address: Option<String>,
    #[serde(default)]
    pub local_port: Option<u16>,
    #[serde(default)]
    pub local_url: Option<String>,
    pub tunnel_pid: Option<u32>,
    pub remote_daemon_pid: Option<u32>,
    pub homeboy_version: String,
    #[serde(default)]
    pub homeboy_build_identity: Option<String>,
    pub connected_at: String,
    #[serde(default)]
    pub worker_identity: Option<String>,
    #[serde(default)]
    pub worker_pid: Option<u32>,
    #[serde(default)]
    pub last_seen_at: Option<String>,
}

impl RunnerSession {
    pub fn lifecycle_owner(&self) -> RunnerLifecycleOwner {
        match self.role {
            RunnerSessionRole::Controller => RunnerLifecycleOwner::Controller,
            RunnerSessionRole::Runner => RunnerLifecycleOwner::Runner,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunnerArtifactRef {
    pub artifact_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport: Option<String>,
}

impl From<&JobArtifactMetadata> for RunnerArtifactRef {
    fn from(artifact: &JobArtifactMetadata) -> Self {
        Self {
            artifact_id: artifact.id.clone(),
            name: artifact.name.clone(),
            path: artifact.path.clone(),
            url: artifact.url.clone(),
            mime: artifact.mime.clone(),
            size_bytes: artifact.size_bytes,
            sha256: artifact.sha256.clone(),
            transport: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunnerJob {
    pub runner_id: String,
    pub job_id: String,
    pub operation: String,
    pub status: JobStatus,
    pub command: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    pub source: String,
    pub lifecycle_owner: RunnerLifecycleOwner,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifecycle: Option<crate::core::api_jobs::RunnerJobLifecycleMetadata>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elapsed_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub heartbeat_age_ms: Option<u64>,
    #[serde(flatten)]
    pub claim: crate::core::api_jobs::JobClaimMetadata,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claim_expires_in_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub durable_run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stale_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifecycle_state: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retryable: Option<bool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_refs: Vec<RunnerArtifactRef>,
}

impl From<&ActiveRunnerJobSummary> for RunnerJob {
    fn from(job: &ActiveRunnerJobSummary) -> Self {
        Self {
            runner_id: job.runner_id.clone(),
            job_id: job.job_id.clone(),
            operation: job.operation.clone(),
            status: job.status,
            command: job.command.clone(),
            cwd: job.cwd.clone(),
            source: job.source.clone(),
            lifecycle_owner: match crate::core::api_jobs::RunnerJobSource::from_metadata(
                &job.source,
            )
            .lifecycle_owner()
            {
                crate::core::api_jobs::RunnerJobLifecycleOwner::Broker => {
                    RunnerLifecycleOwner::Broker
                }
                crate::core::api_jobs::RunnerJobLifecycleOwner::Controller => {
                    RunnerLifecycleOwner::Controller
                }
            },
            lifecycle: job.lifecycle.clone(),
            started_at_ms: Some(job.started_at_ms),
            updated_at_ms: Some(job.updated_at_ms),
            elapsed_ms: Some(job.elapsed_ms),
            heartbeat_age_ms: Some(job.heartbeat_age_ms),
            claim: job.claim.clone(),
            claim_expires_in_ms: job.claim_expires_in_ms,
            durable_run_id: job.durable_run_id.clone(),
            stale_reason: job.stale_reason.clone(),
            lifecycle_state: job.lifecycle_state.clone(),
            retryable: job.retryable,
            artifact_refs: Vec::new(),
        }
    }
}

impl RunnerJob {
    pub fn from_job(
        runner_id: &str,
        source: &str,
        command: &[String],
        cwd: Option<String>,
        job: &Job,
    ) -> Self {
        Self {
            runner_id: runner_id.to_string(),
            job_id: job.id.to_string(),
            operation: job.operation.clone(),
            status: job.status,
            command: redact_argv_display(command),
            cwd,
            source: source.to_string(),
            lifecycle_owner: if source == "broker" {
                RunnerLifecycleOwner::Broker
            } else {
                RunnerLifecycleOwner::Controller
            },
            lifecycle: None,
            started_at_ms: job.started_at_ms,
            updated_at_ms: Some(job.updated_at_ms),
            elapsed_ms: None,
            heartbeat_age_ms: None,
            claim: crate::core::api_jobs::JobClaimMetadata {
                claim_id: job.claim_id.clone(),
                claimed_by_runner_id: job.claimed_by_runner_id.clone(),
                claimed_at_ms: job.claimed_at_ms,
                claim_expires_at_ms: job.claim_expires_at_ms,
            },
            claim_expires_in_ms: None,
            durable_run_id: None,
            stale_reason: job.stale_reason.clone(),
            lifecycle_state: job.stale_reason.as_ref().map(|_| "stale".to_string()),
            retryable: job.stale_reason.as_ref().map(|_| true),
            artifact_refs: job.artifacts.iter().map(RunnerArtifactRef::from).collect(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunnerWorkspaceLease {
    pub runner_id: String,
    pub local_path: String,
    pub remote_path: String,
    pub sync_mode: String,
    pub materialized: bool,
    pub lifecycle_owner: RunnerLifecycleOwner,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_commit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_dirty: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunnerNamedWorkspaceLease {
    pub name: String,
    pub lease: RunnerWorkspaceLease,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct RunnerWorkspaceLeaseSet {
    pub primary: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub leases: Vec<RunnerNamedWorkspaceLease>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct RunnerMutationArtifacts {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub patch_ref: Option<RunnerArtifactRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_bundle_ref: Option<RunnerArtifactRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_log_ref: Option<RunnerArtifactRef>,
}

impl RunnerMutationArtifacts {
    pub fn is_empty(&self) -> bool {
        self.patch_ref.is_none()
            && self.file_bundle_ref.is_none()
            && self.operation_log_ref.is_none()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunnerResult {
    pub exit_code: i32,
    pub status: JobStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdout_bytes: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stderr_bytes: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mirror_run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mutation_artifacts: Option<RunnerMutationArtifacts>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_refs: Vec<RunnerArtifactRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunnerHandoff {
    pub runner_id: String,
    pub transport: String,
    pub lifecycle_owner: RunnerLifecycleOwner,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job: Option<RunnerJob>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_lease: Option<RunnerWorkspaceLease>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_leases: Option<RunnerWorkspaceLeaseSet>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<RunnerResult>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<RunnerTunnelMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<RunnerSessionRole>,
    pub connected: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recorded: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub broker_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub controller_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_daemon_address: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tunnel_pid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_daemon_pid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub homeboy_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub homeboy_build_identity: Option<String>,
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
    pub state: RunnerSessionState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session: Option<RunnerSession>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stale_daemon: Option<RunnerStaleDaemonWarning>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub active_jobs: Vec<ActiveRunnerJobSummary>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub active_runner_jobs: Vec<RunnerJob>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stale_runner_jobs: Vec<RunnerJob>,
    #[serde(default)]
    pub active_job_count: usize,
    #[serde(default)]
    pub stale_runner_job_count: usize,
    pub active_job_state: RunnerActiveJobState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_job_source: Option<RunnerActiveJobSource>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_job_error: Option<RunnerActiveJobError>,
    pub session_path: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RunnerStaleDaemonWarning {
    pub severity: &'static str,
    pub session_homeboy_version: String,
    pub current_homeboy_version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_homeboy_build_identity: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_homeboy_build_identity: Option<String>,
    pub active_daemon_control_plane_version: String,
    pub job_command_binary_version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_daemon_control_plane_build_identity: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job_command_binary_build_identity: Option<String>,
    pub refresh_command: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stale_runtime_paths: Vec<RunnerStaleRuntimePath>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changed_runtime_paths: Vec<RunnerChangedRuntimePath>,
    pub message: String,
    pub recovery_commands: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RunnerStaleRuntimePath {
    pub env: String,
    pub path: String,
    pub loaded_fingerprint: String,
    pub current_fingerprint: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RunnerChangedRuntimePath {
    pub env: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub loaded_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub configured_path: Option<String>,
}

impl RunnerStaleDaemonWarning {
    pub fn new(
        runner_id: &str,
        session_homeboy_version: String,
        current_homeboy_version: String,
        session_homeboy_build_identity: Option<String>,
        current_homeboy_build_identity: Option<String>,
    ) -> Self {
        let recovery_commands = vec![
            format!("homeboy runner disconnect {}", runner_id),
            format!("homeboy runner connect {}", runner_id),
        ];
        Self {
            severity: "warning",
            active_daemon_control_plane_version: session_homeboy_version.clone(),
            job_command_binary_version: current_homeboy_version.clone(),
            active_daemon_control_plane_build_identity: session_homeboy_build_identity.clone(),
            job_command_binary_build_identity: current_homeboy_build_identity.clone(),
            session_homeboy_version,
            current_homeboy_version,
            session_homeboy_build_identity,
            current_homeboy_build_identity,
            refresh_command: recovery_commands.join(" && "),
            message: "connected runner daemon control plane was started by a different Homeboy build than the configured job command binary; run refresh_command to restart the active daemon".to_string(),
            stale_runtime_paths: Vec::new(),
            changed_runtime_paths: Vec::new(),
            recovery_commands: vec![
                format!("homeboy runner refresh-homeboy {} --ref main --reconnect", runner_id),
                format!("homeboy runner disconnect {}", runner_id),
                format!("homeboy runner connect {}", runner_id),
            ],
        }
    }

    pub fn with_runtime_paths(
        mut self,
        runner_id: &str,
        stale_runtime_paths: Vec<RunnerStaleRuntimePath>,
        changed_runtime_paths: Vec<RunnerChangedRuntimePath>,
    ) -> Self {
        self.stale_runtime_paths = stale_runtime_paths;
        self.changed_runtime_paths = changed_runtime_paths;
        if !self.stale_runtime_paths.is_empty() || !self.changed_runtime_paths.is_empty() {
            self.message = "connected runner daemon runtime paths are stale; run recovery_commands in order to restart the active daemon after runner-side rebuilds or path changes".to_string();
            self.recovery_commands = vec![
                format!("homeboy runner disconnect {}", runner_id),
                format!("homeboy runner connect {}", runner_id),
            ];
        }
        self
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct RunnerDisconnectReport {
    pub runner_id: String,
    pub disconnected: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session: Option<RunnerSession>,
    pub session_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReverseRunnerConnectOptions {
    pub controller_id: String,
    pub runner_id: String,
    pub broker_url: Option<String>,
}
