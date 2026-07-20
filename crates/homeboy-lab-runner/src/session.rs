use serde::{Deserialize, Serialize};

use homeboy_core::daemon::{
    DaemonFreshnessReport, DaemonLeaselessRecoveryResult, DaemonStateLossRecoveryResult,
};

use homeboy_core::engine::shell;
use homeboy_core::redaction::redact_argv_shell_display;

use homeboy_core::api_jobs::{ActiveRunnerJobSummary, Job, JobArtifactMetadata, JobStatus};

mod session_enums {
    use super::*;

    // RunnerLifecycleOwner now lives in the shared runner-contract crate so core
    // (dev_run, run_outcome_envelope, and the types that reference it) can name it
    // without a core -> runner edge. Re-exported so runner-internal call sites
    // resolve unchanged.
    pub use homeboy_lab_runner_contract::RunnerLifecycleOwner;

    // Session data types (RunnerTunnelMode, RunnerSessionRole, RunnerSessionState,
    // RunnerSession) now live in homeboy-runner-contract so the core daemon can
    // build/persist sessions without a core -> runner edge. Re-exported so
    // runner-internal call sites resolve unchanged.
    pub use homeboy_lab_runner_contract::{
        RunnerSession, RunnerSessionRole, RunnerSessionState, RunnerTunnelMode,
    };

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

/// The authoritative runner condition used by callers that need to decide
/// whether to reconnect, wait for capacity, or recover a stale daemon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunnerRecoveryState {
    Disconnected,
    StaleDaemon { active_job_count: usize },
    Busy { active_job_count: usize },
    Idle,
}

impl RunnerAvailability {
    /// A connected runner with an authoritative capacity limit may accept work
    /// into its durable broker queue even though it cannot start it yet.
    pub fn is_capacity_exhausted(&self) -> bool {
        self.reasons == ["capacity_reached"]
    }

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

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RunnerActiveJobRecoveryEvidence {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reconciled_job_ids: Vec<String>,
    pub prior_active_job_count: usize,
    pub active_job_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunnerLeaselessRecoveryContract {
    ConfirmNoDaemonOwner,
    ReconcileLeaselessOrphansAndConfirmNoDaemonOwner,
    ConfirmControlPlaneLost,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunnerLeaselessRecoveryEvidence {
    pub contract: RunnerLeaselessRecoveryContract,
    pub remote_command_identity: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery: Option<DaemonLeaselessRecoveryResult>,
}

// RunnerArtifactRef is behavior-free data; it now lives in the shared
// runner-contract crate so core can name it without a core -> runner edge.
// Re-exported so internal/CLI call sites resolve unchanged. The
// From<&JobArtifactMetadata> impl stays below (orphan rule: JobArtifactMetadata
// is core-owned).
pub use homeboy_lab_runner_contract::RunnerArtifactRef;

/// Build a `RunnerArtifactRef` from a core job artifact. A free function rather
/// than a `From` impl because both types are foreign to this crate (orphan rule).
pub(crate) fn runner_artifact_ref_from_metadata(
    artifact: &JobArtifactMetadata,
) -> RunnerArtifactRef {
    RunnerArtifactRef {
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
    pub lifecycle: Option<homeboy_core::api_jobs::RunnerJobLifecycleMetadata>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elapsed_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub heartbeat_age_ms: Option<u64>,
    #[serde(flatten)]
    pub claim: homeboy_core::api_jobs::JobClaimMetadata,
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
            lifecycle_owner: match homeboy_core::api_jobs::RunnerJobSource::from_metadata(
                &job.source,
            )
            .lifecycle_owner()
            {
                homeboy_core::api_jobs::RunnerJobLifecycleOwner::Broker => {
                    RunnerLifecycleOwner::Broker
                }
                homeboy_core::api_jobs::RunnerJobLifecycleOwner::Controller => {
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
            command: redact_argv_shell_display(command),
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
            claim: homeboy_core::api_jobs::JobClaimMetadata {
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
            artifact_refs: job
                .artifacts
                .iter()
                .map(crate::session::runner_artifact_ref_from_metadata)
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_runner_job_projection_uses_redacted_shell_display() {
        let job = Job {
            id: uuid::Uuid::nil(),
            operation: "runner.exec".to_string(),
            status: JobStatus::Running,
            created_at_ms: 1,
            updated_at_ms: 1,
            started_at_ms: Some(1),
            finished_at_ms: None,
            event_count: 0,
            source_snapshot: None,
            path_materialization_plan: None,
            stale_reason: None,
            daemon_lease_id: None,
            target_runner_id: None,
            target_project_id: None,
            claim_id: None,
            claimed_by_runner_id: None,
            claimed_at_ms: None,
            claim_expires_at_ms: None,
            artifacts: Vec::new(),
            runner_job_projection: None,
        };
        let command = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "run".to_string(),
            "#8949".to_string(),
            "path with spaces".to_string(),
            "O'Brien".to_string(),
            "$HOME/work".to_string(),
            "--provider-auth-token".to_string(),
            "secret-value".to_string(),
        ];

        let projection = RunnerJob::from_job("homeboy-lab", "daemon", &command, None, &job);

        assert_eq!(
            projection.command,
            "homeboy agent-task run '#8949' 'path with spaces' 'O'\\''Brien' '$HOME/work' --provider-auth-token '[REDACTED]'"
        );
        assert!(!projection.command.contains("secret-value"));
    }
}

// RunnerWorkspaceLease now lives in the shared runner-contract crate (core's
// dev_run names it). Re-exported so runner-internal call sites resolve.
pub use homeboy_lab_runner_contract::RunnerWorkspaceLease;

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

pub use homeboy_lab_runner_contract::RunnerMutationArtifacts;

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
pub struct LabRunnerHandoff {
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
    /// Drift observed while reattaching the exact remote daemon lease and PID.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connection_warning: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub homeboy_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub homeboy_build_identity: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub leaseless_recovery: Option<DaemonLeaselessRecoveryResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state_loss_recovery: Option<DaemonStateLossRecoveryResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub leaseless_recovery_evidence: Option<RunnerLeaselessRecoveryEvidence>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub daemon_freshness: Option<DaemonFreshnessReport>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_job_recovery_evidence: Option<RunnerActiveJobRecoveryEvidence>,
    pub session_path: String,
}

impl RunnerStatusReport {
    /// The session state is the canonical connection view; `connected` is a
    /// serialized compatibility field for command consumers.
    pub fn is_connected(&self) -> bool {
        self.state == RunnerSessionState::Connected
    }

    pub fn recovery_state(&self) -> RunnerRecoveryState {
        if !self.is_connected() {
            RunnerRecoveryState::Disconnected
        } else if self.stale_daemon.is_some() {
            RunnerRecoveryState::StaleDaemon {
                active_job_count: self.active_job_count,
            }
        } else if self.active_job_count > 0 {
            RunnerRecoveryState::Busy {
                active_job_count: self.active_job_count,
            }
        } else {
            RunnerRecoveryState::Idle
        }
    }
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
        let recovery_commands = vec![format!(
            "homeboy runner refresh-homeboy {} --ref v{} --reconnect",
            shell::quote_arg(runner_id),
            homeboy_product_identity::product_version()
        )];
        let message = if !same_homeboy_version(&session_homeboy_version, &current_homeboy_version) {
            format!(
                "connected runner daemon control plane version `{session_homeboy_version}` differs from configured job command binary version `{current_homeboy_version}`; run recovery_commands in order when runner active jobs are drained"
            )
        } else if let (Some(session_identity), Some(current_identity)) = (
            session_homeboy_build_identity.as_deref(),
            current_homeboy_build_identity.as_deref(),
        ) {
            format!(
                "connected runner daemon control plane build identity `{session_identity}` differs from configured job command binary build identity `{current_identity}`; run recovery_commands in order when runner active jobs are drained"
            )
        } else {
            "connected runner daemon runtime requires explicit identity verification; run recovery_commands in order when runner active jobs are drained".to_string()
        };
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
            message,
            stale_runtime_paths: Vec::new(),
            changed_runtime_paths: Vec::new(),
            recovery_commands,
        }
    }

    pub fn with_identity_unverifiable(
        mut self,
        runner_id: &str,
        configured_executable: &str,
        unverifiable: bool,
    ) -> Self {
        if unverifiable {
            self.message = format!(
                "connected runner daemon build identity could not be verified against configured executable `{configured_executable}`; run `{} self identity` on the runner and ensure it reports `git_commit` or an exact `display`, then run recovery_commands if needed",
                shell::quote_arg(configured_executable),
            );
            self.recovery_commands = vec![format!(
                "homeboy runner refresh-homeboy {} --reconnect",
                shell::quote_arg(runner_id),
            )];
            self.refresh_command = self.recovery_commands.join(" && ");
        }
        self
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
            let stale_paths = self.stale_runtime_paths.iter().map(|path| {
                format!(
                    "{} at `{}` fingerprint `{}` -> `{}`",
                    path.env, path.path, path.loaded_fingerprint, path.current_fingerprint
                )
            });
            let changed_paths = self.changed_runtime_paths.iter().map(|path| {
                format!(
                    "{} path `{:?}` -> `{:?}`",
                    path.env, path.loaded_path, path.configured_path
                )
            });
            self.message = format!(
                "connected runner daemon runtime paths are stale: {}; run recovery_commands after active jobs are drained to replace the active daemon with the configured runtime paths",
                stale_paths.chain(changed_paths).collect::<Vec<_>>().join("; ")
            );
            self.recovery_commands = vec![format!(
                "homeboy runner refresh-homeboy {} --ref v{} --reconnect",
                shell::quote_arg(runner_id),
                homeboy_product_identity::product_version()
            )];
        }
        self
    }
}

fn same_homeboy_version(left: &str, right: &str) -> bool {
    left.trim().strip_prefix("homeboy ").unwrap_or(left.trim())
        == right
            .trim()
            .strip_prefix("homeboy ")
            .unwrap_or(right.trim())
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
