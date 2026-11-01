use chrono::{DateTime, Utc};
use serde::ser::{SerializeStruct, Serializer};
use serde::{Deserialize, Serialize};

use homeboy_core::daemon::{
    DaemonFreshnessReport, DaemonLeaselessRecoveryResult, DaemonStateLossRecoveryResult,
};

use homeboy_core::engine::shell;
use homeboy_core::error::{ActionSafety, ExecutableAction};
use homeboy_core::redaction::redact_argv_shell_display;

use homeboy_core::api_jobs::{ActiveRunnerJobSummary, Job, JobArtifactMetadata, JobStatus};

mod session_enums {
    use super::*;

    // RunnerLifecycleOwner now lives in the shared runner-contract crate so core
    // (dev_run, run_outcome_envelope, and the types that reference it) can name it
    // without a core -> runner edge. Re-exported so runner-internal call sites
    // resolve unchanged.
    pub use homeboy_runner_contract::RunnerLifecycleOwner;

    // Session data types (RunnerTunnelMode, RunnerSessionRole, RunnerSessionState,
    // RunnerSession) now live in homeboy-runner-contract so the core daemon can
    // build/persist sessions without a core -> runner edge. Re-exported so
    // runner-internal call sites resolve unchanged.
    pub use homeboy_runner_contract::{
        RunnerProxyForward, RunnerSession, RunnerSessionRole, RunnerSessionState, RunnerTunnelMode,
        RunnerTunnelProcessStartIdentity,
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
pub use homeboy_runner_contract::RunnerArtifactRef;

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
            lifecycle_owner: homeboy_core::api_jobs::RunnerJobSource::from_metadata(&job.source)
                .lifecycle_owner(),
            lifecycle: job.lifecycle.clone(),
            started_at_ms: Some(job.started_at_ms),
            updated_at_ms: Some(job.updated_at_ms),
            elapsed_ms: Some(job.elapsed_ms),
            heartbeat_age_ms: Some(job.heartbeat_age_ms),
            claim: job.claim.clone(),
            claim_expires_in_ms: job.claim_expires_in_ms,
            durable_run_id: job.durable_run_id.clone(),
            stale_reason: job.stale_reason.clone(),
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

pub use homeboy_runner_contract::RunnerWorkspaceLease;

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

pub use homeboy_runner_contract::RunnerMutationArtifacts;

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
    TransportUnreachable,
    RunnerCapabilityMissing,
    DaemonStartupFailure,
    TunnelFailure,
    /// A daemon in the pre-#16752 legacy state-dir layout still has
    /// active jobs, so connect refuses to start a second daemon beside it.
    LegacyDaemonBusy,
}

/// Bounded evidence retained when a direct runner connect cannot complete.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunnerConnectFailureEvidenceRef {
    pub schema_version: u8,
    pub run_id: String,
    pub artifact_id: String,
    pub uri: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunnerConnectFailureEvidence {
    pub recovery_command: String,
    pub classification: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_start_command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub known_remote_pid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub known_remote_lease_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_address: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_address: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tunnel_state: Option<String>,
    /// The connect lifecycle stage that produced this evidence. Older callers
    /// can continue reading the existing health fields.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub durability_stage: Option<String>,
    /// Failed initial health probes and failed durability observations only;
    /// successful health observations are intentionally omitted.
    pub health_attempt_count: usize,
    pub health_attempts: Vec<String>,
    /// Typed reference to complete redacted remote failure evidence.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_evidence_ref: Option<RunnerConnectFailureEvidenceRef>,
}

/// How runner-binary convergence reconciled the runner's configured Homeboy
/// binary with a target controller release: `homeboy runner connect`'s own
/// build, or the controller `homeboy upgrade --upgrade-runner` installed.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunnerBinaryConvergenceStatus {
    /// The controller's release was installed and the daemon replaced.
    Converged,
    /// The configured binary already runs the controller's release version.
    AlreadyCurrent,
    /// The daemon has active jobs; a later convergence installs the release.
    PendingActiveJobs,
    /// The controller is not a published release build.
    SkippedDevBuild,
    /// The configured binary is newer than the controller; never downgraded.
    SkippedRemoteNewer,
    /// The configured binary's identity could not be read.
    SkippedUnverifiable,
    /// Installing the controller's release failed; the existing binary stays configured.
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunnerBinaryConvergence {
    pub status: RunnerBinaryConvergenceStatus,
    /// The configured runner binary's identity before convergence.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    /// The controller release identity convergence targets (or installed).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Active daemon jobs that hold convergence pending.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocking_job_ids: Vec<String>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_evidence: Option<RunnerConnectFailureEvidence>,
    /// Present only for `homeboy runner connect`, which converges the runner
    /// binary to the controller's release before attaching.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub binary_convergence: Option<RunnerBinaryConvergence>,
}

/// A read-only indexed snapshot of one runner's persisted active jobs, built
/// without reconciling (polling) the daemon. Used by latency-bounded
/// callers (#9522).
#[derive(Debug, Clone)]
pub struct RunnerActiveJobsSnapshot {
    pub runner_id: String,
    pub connected: bool,
    pub active_jobs: Vec<ActiveRunnerJobSummary>,
    pub active_job_state: RunnerActiveJobState,
    pub active_job_error: Option<RunnerActiveJobError>,
}

#[derive(Debug, Clone)]
pub struct RunnerStatusReport {
    pub runner_id: String,
    pub connected: bool,
    pub state: RunnerSessionState,
    pub session: Option<RunnerSession>,
    pub stale_daemon: Option<RunnerStaleDaemonWarning>,
    /// Immutable identity read from the configured job binary by the
    /// authoritative direct-SSH status probe.
    pub configured_job_binary_build_identity: Option<String>,
    pub daemon_freshness: Option<DaemonFreshnessReport>,
    pub active_jobs: Vec<ActiveRunnerJobSummary>,
    pub active_runner_jobs: Vec<RunnerJob>,
    pub stale_runner_jobs: Vec<RunnerJob>,
    pub active_job_count: usize,
    pub stale_runner_job_count: usize,
    pub active_job_state: RunnerActiveJobState,
    pub active_job_source: Option<RunnerActiveJobSource>,
    pub active_job_error: Option<RunnerActiveJobError>,
    pub active_job_recovery_evidence: Option<RunnerActiveJobRecoveryEvidence>,
    pub session_path: String,
}

impl Serialize for RunnerStatusReport {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        // Serialize the captured observation only. Reading runner state again
        // here would create a second point-in-time truth while rendering an
        // already observed status.
        let mut state = serializer.serialize_struct("RunnerStatusReport", 18)?;
        state.serialize_field("runner_id", &self.runner_id)?;
        state.serialize_field("connected", &self.connected)?;
        state.serialize_field("state", &self.state)?;
        if let Some(session) = &self.session {
            state.serialize_field("session", session)?;
        }
        if let Some(stale_daemon) = &self.stale_daemon {
            state.serialize_field("stale_daemon", stale_daemon)?;
        }
        if let Some(identity) = &self.configured_job_binary_build_identity {
            state.serialize_field("configured_job_binary_build_identity", identity)?;
        }
        if let Some(daemon_freshness) = &self.daemon_freshness {
            state.serialize_field("daemon_freshness", daemon_freshness)?;
        }
        if !self.active_jobs.is_empty() {
            state.serialize_field("active_jobs", &self.active_jobs)?;
        }
        if !self.active_runner_jobs.is_empty() {
            state.serialize_field("active_runner_jobs", &self.active_runner_jobs)?;
        }
        if !self.stale_runner_jobs.is_empty() {
            state.serialize_field("stale_runner_jobs", &self.stale_runner_jobs)?;
        }
        if self.active_job_state == RunnerActiveJobState::Available {
            state.serialize_field("active_job_count", &self.active_job_count)?;
        }
        state.serialize_field("stale_runner_job_count", &self.stale_runner_job_count)?;
        state.serialize_field("active_job_state", &self.active_job_state)?;
        if let Some(active_job_source) = &self.active_job_source {
            state.serialize_field("active_job_source", active_job_source)?;
        }
        if let Some(active_job_error) = &self.active_job_error {
            state.serialize_field("active_job_error", active_job_error)?;
        }
        if let Some(evidence) = &self.active_job_recovery_evidence {
            state.serialize_field("active_job_recovery_evidence", evidence)?;
        }
        state.serialize_field("session_path", &self.session_path)?;
        state.end()
    }
}

/// The canonical lease/freshness observation used by admission and status.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct RunnerDaemonLeaseVerdict {
    /// The daemon lease identified by the live report, or the lease in the
    /// persisted controller session when the live report has no lease
    /// identity.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generation_identity_source: Option<&'static str>,
    /// Lease observation timestamp captured by the controller session.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observed_at: Option<String>,
    /// Age of the captured session observation when the verdict was projected.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observation_age_ms: Option<u64>,
    pub freshness_observed: bool,
    pub lease_published: bool,
    pub fresh: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_action: Option<String>,
}

/// One compact, authoritative answer to "is this runner ready for the next
/// workload right now, and is it safe to rotate?" — the lead of `runner
/// status`.
///
/// It is derived from the runner's one current session and the selected
/// daemon's live job view, so it stays small and fast (#9478/#9522).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RunnerAdmissionSummary {
    pub runner_id: String,
    /// Whether the runner's session is connected.
    pub connected: bool,
    /// Whether the daemon freshness predicate permits admission. An absent
    /// observation remains unverified rather than stale.
    pub daemon_fresh: bool,
    /// Shared lease identity, freshness, observation-age, and recovery verdict.
    pub daemon_lease_verdict: RunnerDaemonLeaseVerdict,
    /// Whether the selected daemon and configured job binary are compatible
    /// with the controller for admission.
    pub daemon_compatible: bool,
    /// Whether the runner can admit a new workload now: connected, fresh, and
    /// not otherwise blocked.
    pub accepting_jobs: bool,
    /// Runner-owned live count; absent when the job observation is unavailable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_job_count: Option<usize>,
    /// Jobs currently observed by the selected daemon.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub live_daemon_job_count: Option<usize>,
    /// Count of runner jobs whose liveness can no longer be confirmed.
    pub stale_job_count: usize,
    /// The exact build identity of the selected daemon, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub daemon_build_identity: Option<String>,
    /// Whether the selected daemon can be safely replaced now. A known stale
    /// version is rotatable after its typed job view proves idle; ambiguous
    /// lease ownership is not.
    pub safe_to_rotate: bool,
    /// The single next actionable step, state-sensitive and executable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_action: Option<String>,
}

impl RunnerStatusReport {
    /// The session state is the canonical connection view; `connected` is a
    /// serialized compatibility field for command consumers.
    pub fn is_connected(&self) -> bool {
        self.state == RunnerSessionState::Connected
    }

    /// The stale-daemon report that fences admission, if any.
    pub fn admission_blocking_stale_daemon(&self) -> Option<&RunnerStaleDaemonWarning> {
        self.stale_daemon.as_ref().filter(|warning| {
            warning.verification != RunnerDaemonVerification::Compared
                || !warning.stale_runtime_paths.is_empty()
                || !warning.changed_runtime_paths.is_empty()
        })
    }

    /// Whether an observed fresh daemon has a lease identity for admission.
    /// Missing freshness or lease identity is unverified and fails closed.
    pub fn daemon_fresh(&self) -> bool {
        self.daemon_lease_verdict(None).fresh
    }

    /// Project the one lease/freshness answer used by admission surfaces.
    pub fn daemon_lease_verdict(&self, next_action: Option<String>) -> RunnerDaemonLeaseVerdict {
        let observed_freshness = self.daemon_freshness.as_ref();
        let reported_lease = observed_freshness
            .and_then(|freshness| freshness.lease_id.as_deref())
            .filter(|lease| !lease.is_empty());
        let persisted_lease = self
            .session
            .as_ref()
            .and_then(|session| session.remote_daemon_lease_id.as_deref())
            .filter(|lease| !lease.is_empty());
        let observed_at = self.session.as_ref().and_then(|session| {
            session
                .last_seen_at
                .as_deref()
                .filter(|value| DateTime::parse_from_rfc3339(value).is_ok())
                .or_else(|| {
                    DateTime::parse_from_rfc3339(&session.connected_at)
                        .is_ok()
                        .then_some(session.connected_at.as_str())
                })
                .map(str::to_string)
        });
        let observation_age_ms = observed_at
            .as_deref()
            .and_then(|observed| DateTime::parse_from_rfc3339(observed).ok())
            .map(|observed| {
                (Utc::now() - observed.with_timezone(&Utc))
                    .num_milliseconds()
                    .max(0) as u64
            });
        RunnerDaemonLeaseVerdict {
            generation_id: reported_lease.or(persisted_lease).map(str::to_string),
            generation_identity_source: reported_lease
                .map(|_| "daemon_freshness_report")
                .or_else(|| persisted_lease.map(|_| "persisted_session")),
            observed_at,
            observation_age_ms,
            freshness_observed: observed_freshness.is_some(),
            lease_published: reported_lease.is_some(),
            fresh: observed_freshness.is_some_and(|freshness| freshness.fresh)
                && reported_lease.is_some(),
            next_action,
        }
    }

    /// Whether the selected daemon is compatible with the controller and
    /// configured job binary for admission.
    pub fn daemon_compatible_for_admission(&self) -> bool {
        self.admission_blocking_stale_daemon().is_none()
    }

    /// The admission freshness fence shared by placement and dispatch.
    /// Compatibility and the daemon's own freshness observation both prohibit
    /// new work; rotation safety is evaluated separately.
    ///
    /// Missing freshness or lease identity is unverified and cannot authorize
    /// admission; callers surface recovery guidance from the same observation.
    pub fn daemon_fresh_for_admission(&self) -> bool {
        self.daemon_compatible_for_admission() && self.daemon_fresh()
    }

    /// Capacity remains owned by `RunnerAvailability`; this method only adds
    /// the daemon freshness fact so placement does not duplicate capacity logic
    /// introduced by the aggregate runner accounting path (#9474).
    pub fn admission_availability(&self, capacity: Option<usize>) -> RunnerAvailability {
        let mut availability = RunnerAvailability::from_status_parts(
            self.runner_id.clone(),
            self.is_connected(),
            self.admission_blocking_stale_daemon().is_some(),
            // `active_job_count` is the selected daemon's authoritative live
            // count; the typed view can transiently expose more identities.
            self.active_job_count.max(self.active_jobs.len()),
            &self.active_job_state,
            capacity,
        );
        if !self.daemon_fresh_for_admission() && self.admission_blocking_stale_daemon().is_none() {
            availability
                .reasons
                .push("daemon_freshness_unavailable".to_string());
            availability.accepts_jobs = false;
        }
        availability
    }

    /// Project the authoritative current-state admission summary from the
    /// supplied live report without reading or mutating any external state.
    pub fn admission_summary(&self) -> RunnerAdmissionSummary {
        let connected = self.is_connected();
        // Compatibility and an explicitly non-fresh or lease-less observation
        // both fence admission.
        let mut daemon_lease_verdict = self.daemon_lease_verdict(None);
        let daemon_fresh = daemon_lease_verdict.fresh;
        let daemon_compatible = self.daemon_compatible_for_admission();
        // A persisted session proves neither current daemon liveness nor its
        // active-job count. Admission and rotation remain fail-closed until a
        // current job view is available.
        let active_jobs_available = self.active_job_state == RunnerActiveJobState::Available;
        let active_job_count = self.active_job_count;
        let accepting_jobs =
            connected && daemon_fresh && daemon_compatible && active_jobs_available;
        let safe_to_rotate = connected
            && self.rotation_evidence_is_unambiguous()
            && active_jobs_available
            && active_job_count == 0;
        let daemon_build_identity = self
            .session
            .as_ref()
            .and_then(|session| session.homeboy_build_identity.clone());

        // Terminal ownership uncertainty has no authorized mutation to
        // prescribe, so it suppresses every admission action.
        let next_action = (!self
            .daemon_freshness
            .as_ref()
            .is_some_and(DaemonFreshnessReport::has_terminal_recovery_ownership_blocker))
        .then(|| self.admission_action())
        .flatten()
        // A rotation the live jobs also block is a dead end (#15995): the
        // refresh refuses until they drain. Point at the jobs instead.
        .map(|action| {
            let rotation_blocked_by_live_jobs = action.id == "runner.refresh_homeboy"
                && connected
                && active_jobs_available
                && active_job_count > 0
                && !safe_to_rotate;
            if rotation_blocked_by_live_jobs {
                job_list_action(&self.runner_id)
            } else {
                action
            }
        })
        .map(|action| action.render_command());
        daemon_lease_verdict.next_action = next_action.clone();

        RunnerAdmissionSummary {
            runner_id: self.runner_id.clone(),
            connected,
            daemon_fresh,
            daemon_lease_verdict,
            daemon_compatible,
            accepting_jobs,
            active_job_count: active_jobs_available.then_some(active_job_count),
            live_daemon_job_count: active_jobs_available.then_some(active_job_count),
            stale_job_count: self.stale_runner_job_count,
            daemon_build_identity,
            safe_to_rotate,
            next_action,
        }
    }

    /// Typed action for a state that blocks runner admission.
    ///
    /// The compact status projection renders this action, while the full
    /// operator projection uses [`Self::status_action`] below. Keeping argv as
    /// the source prevents either surface from dropping the runner id.
    pub fn admission_action(&self) -> Option<ExecutableAction> {
        if let Some(freshness) = self.daemon_freshness.as_ref() {
            if freshness.has_terminal_recovery_ownership_blocker() {
                return None;
            }
            if let Some(action) = freshness
                .repair_plan
                .first()
                .and_then(|step| step.action.clone())
            {
                return Some(action);
            }
        }
        if self.is_connected() && !self.daemon_fresh() {
            return Some(status_action(&self.runner_id));
        }
        if let Some(warning) = self.admission_blocking_stale_daemon() {
            if let Some(action) = warning.safe_recovery_actions().into_iter().next() {
                return Some(action);
            }
        }
        if let Some(warning) = &self.stale_daemon {
            warning.safe_recovery_actions().into_iter().next()
        } else if !self.is_connected() {
            Some(crate::daemon_repair::connect_action(&self.runner_id))
        } else if self.active_job_state != RunnerActiveJobState::Available {
            Some(status_action(&self.runner_id))
        } else {
            None
        }
    }

    /// Admission freshness and replacement safety are intentionally distinct:
    /// version drift blocks new work but an identified, authoritatively idle
    /// daemon may be rotated to converge. Missing or corrupt lease state has no
    /// such owner proof and must remain fenced.
    pub(crate) fn rotation_evidence_is_unambiguous(&self) -> bool {
        let Some(freshness) = self.daemon_freshness.as_ref() else {
            return false;
        };
        match (freshness.fresh, freshness.stale_reason_code) {
            // A current daemon observation with no stale predicate is an
            // explicit proof that rotation may proceed once the job view is
            // drained.
            (true, None) => true,
            // These identity/runtime mismatches identify a live daemon without
            // undermining its lease ownership. Require both coordinates before
            // allowing the stale-but-idle convergence path (#9474).
            (
                false,
                Some(
                    homeboy_core::daemon::DaemonStaleReasonCode::VersionMismatch
                    | homeboy_core::daemon::DaemonStaleReasonCode::BuildIdentityMismatch
                    | homeboy_core::daemon::DaemonStaleReasonCode::BinaryHashMismatch
                    | homeboy_core::daemon::DaemonStaleReasonCode::RuntimePathsDrift,
                ),
            ) => {
                freshness
                    .lease_id
                    .as_deref()
                    .is_some_and(|lease_id| !lease_id.is_empty())
                    && freshness.pid.is_some()
            }
            // A missing code on a non-fresh report and every present or future
            // code not allowlisted above leave ownership ambiguous.
            _ => false,
        }
    }

    /// Typed next action for an operator status row.
    ///
    /// A healthy or busy runner has no admission remediation, so re-inspecting
    /// its full status is the safe operator follow-up.
    pub fn status_action(&self) -> ExecutableAction {
        self.admission_action()
            .unwrap_or_else(|| status_action(&self.runner_id))
    }

    pub fn recovery_state(&self) -> RunnerRecoveryState {
        if !self.is_connected() {
            RunnerRecoveryState::Disconnected
        } else if self.admission_blocking_stale_daemon().is_some() {
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

fn status_action(runner_id: &str) -> ExecutableAction {
    ExecutableAction::new(
        "runner.status",
        format!("inspect runner {runner_id} status"),
        "homeboy",
        [
            "runner".to_string(),
            "status".to_string(),
            runner_id.to_string(),
            "--full".to_string(),
        ],
        ActionSafety::ReadOnly,
    )
}

fn job_list_action(runner_id: &str) -> ExecutableAction {
    ExecutableAction::new(
        "runner.job_list",
        format!(
            "list live jobs on runner {runner_id}; wait for them or cancel them before rotating"
        ),
        "homeboy",
        [
            "runner".to_string(),
            "job".to_string(),
            "list".to_string(),
            runner_id.to_string(),
        ],
        ActionSafety::ReadOnly,
    )
}

#[cfg(test)]
mod status_serialization_tests {
    use super::*;

    fn base_report() -> RunnerStatusReport {
        RunnerStatusReport {
            runner_id: "homeboy-lab".to_string(),
            connected: true,
            state: RunnerSessionState::Connected,
            session: None,
            stale_daemon: None,
            configured_job_binary_build_identity: None,
            daemon_freshness: None,
            active_jobs: Vec::new(),
            active_runner_jobs: Vec::new(),
            stale_runner_jobs: Vec::new(),
            active_job_count: 0,
            stale_runner_job_count: 0,
            active_job_state: RunnerActiveJobState::NotQueried,
            active_job_source: None,
            active_job_error: None,
            active_job_recovery_evidence: None,
            session_path: "test".to_string(),
        }
    }

    fn fresh_daemon_freshness() -> DaemonFreshnessReport {
        DaemonFreshnessReport {
            fresh: true,
            stale_reason_code: None,
            restartable: true,
            lease_id: Some("known-lease".to_string()),
            pid: Some(1234),
            recovery_evidence: None,
            ownership_evidence: None,
            adoption_command: None,
            binary_hash: None,
            daemon_version: None,
            daemon_build_identity: None,
            runtime_paths: None,
            active_jobs: 0,
            termination_evidence: None,
            repair_plan: Vec::new(),
        }
    }

    fn session_reporting_version(version: &str) -> RunnerSession {
        RunnerSession {
            runner_id: "homeboy-lab".to_string(),
            mode: RunnerTunnelMode::DirectSsh,
            role: RunnerSessionRole::Controller,
            server_id: None,
            controller_id: None,
            broker_url: None,
            remote_daemon_address: None,
            local_port: None,
            local_url: None,
            tunnel_pid: None,
            tunnel_process_start_identity: None,
            proxy_forward: None,
            remote_daemon_pid: Some(42),
            remote_daemon_lease_id: Some("lease".to_string()),
            homeboy_version: version.to_string(),
            homeboy_build_identity: Some(format!("homeboy {version}+0123456789ab")),
            connected_at: "2026-09-24T00:00:00Z".to_string(),
            worker_identity: Some("worker".to_string()),
            worker_pid: Some(43),
            last_seen_at: Some("2026-09-24T00:00:01Z".to_string()),
            leaseless_recovery_evidence: None,
        }
    }

    fn minor_skewed_version() -> String {
        let controller = homeboy_product_identity::product_version();
        let mut parts = controller.split('.');
        let major = parts.next().expect("major");
        let minor = parts
            .next()
            .and_then(|part| part.parse::<u64>().ok())
            .expect("minor");
        format!("{major}.{}.0", minor.wrapping_add(1))
    }

    #[test]
    fn status_serialization_uses_captured_fields_and_omits_unknown_counts() {
        let report = RunnerStatusReport {
            runner_id: "missing-runner".to_string(),
            connected: false,
            state: RunnerSessionState::Disconnected,
            session: None,
            stale_daemon: None,
            configured_job_binary_build_identity: None,
            daemon_freshness: None,
            active_jobs: Vec::new(),
            active_runner_jobs: Vec::new(),
            stale_runner_jobs: Vec::new(),
            active_job_count: 0,
            stale_runner_job_count: 0,
            active_job_state: RunnerActiveJobState::NotQueried,
            active_job_source: None,
            active_job_error: None,
            active_job_recovery_evidence: None,
            session_path: "test".to_string(),
        };

        let value = serde_json::to_value(report).expect("serialize status");
        for field in [
            "session",
            "stale_daemon",
            "daemon_freshness",
            "active_jobs",
            "active_runner_jobs",
            "stale_runner_jobs",
            "active_job_source",
            "active_job_error",
            "active_job_recovery_evidence",
        ] {
            assert!(value.get(field).is_none(), "{field} must be omitted");
        }
        assert!(
            value.get("active_job_count").is_none(),
            "unqueried is not idle"
        );
    }

    #[test]
    fn matching_configured_binary_evidence_does_not_create_a_stale_warning() {
        let mut report = base_report();
        report.configured_job_binary_build_identity =
            Some("homeboy 0.339.0+83a9bd058619".to_string());

        assert!(report.stale_daemon.is_none());
        assert_eq!(
            report.configured_job_binary_build_identity.as_deref(),
            Some("homeboy 0.339.0+83a9bd058619")
        );
        assert!(!report.daemon_fresh_for_admission());
    }

    #[test]
    fn fresh_flag_without_a_lease_cannot_authorize_admission() {
        let mut report = base_report();
        report.active_job_state = RunnerActiveJobState::Available;
        let mut freshness = fresh_daemon_freshness();
        freshness.lease_id = None;
        report.daemon_freshness = Some(freshness);

        let summary = report.admission_summary();
        assert!(!summary.daemon_fresh);
        assert!(!summary.accepting_jobs);
        assert!(summary.daemon_lease_verdict.freshness_observed);
        assert!(!summary.daemon_lease_verdict.lease_published);
        assert!(!summary.daemon_lease_verdict.fresh);
        assert_eq!(
            summary.next_action.as_deref(),
            Some("homeboy runner status homeboy-lab --full")
        );
        assert_eq!(
            summary.daemon_lease_verdict.next_action, summary.next_action,
            "status and the canonical lease verdict share one recovery action"
        );
    }

    #[test]
    fn admission_summary_projects_lease_and_session_observation_age() {
        let mut report = base_report();
        report.active_job_state = RunnerActiveJobState::Available;
        report.session = Some(session_reporting_version(
            &homeboy_product_identity::product_version(),
        ));
        report.daemon_freshness = Some(fresh_daemon_freshness());

        let verdict = report.admission_summary().daemon_lease_verdict;

        assert!(verdict.freshness_observed);
        assert!(verdict.lease_published);
        assert!(verdict.fresh);
        assert_eq!(verdict.generation_id.as_deref(), Some("known-lease"));
        assert_eq!(
            verdict.generation_identity_source,
            Some("daemon_freshness_report")
        );
        assert_eq!(verdict.observed_at.as_deref(), Some("2026-09-24T00:00:01Z"));
        assert!(verdict.observation_age_ms.is_some_and(|age| age > 0));
    }

    #[test]
    fn reported_build_drift_does_not_block_cook_admission() {
        let mut report = base_report();
        report.active_job_state = RunnerActiveJobState::Available;
        report.daemon_freshness = Some(fresh_daemon_freshness());
        report.session = Some(session_reporting_version(&minor_skewed_version()));

        let summary = report.admission_summary();

        assert!(summary.daemon_compatible);
        assert!(summary.accepting_jobs);
        assert!(summary.next_action.is_none());
    }

    #[test]
    fn admission_summary_fresh_idle_accepts_and_is_safe_to_rotate() {
        let mut report = base_report();
        report.active_job_state = RunnerActiveJobState::Available;
        report.daemon_freshness = Some(fresh_daemon_freshness());
        let summary = report.admission_summary();
        assert!(summary.connected);
        assert!(summary.daemon_fresh);
        assert!(summary.accepting_jobs);
        assert_eq!(summary.active_job_count, Some(0));
        assert!(summary.safe_to_rotate);
        assert_eq!(summary.next_action, None);
    }

    #[test]
    fn admission_summary_unqueried_fails_closed_for_admission_and_rotation() {
        let summary = base_report().admission_summary();

        assert!(!summary.accepting_jobs);
        assert!(!summary.safe_to_rotate);
        assert_eq!(summary.active_job_count, None);
        assert_eq!(summary.live_daemon_job_count, None);
        let wire = serde_json::to_value(&summary).expect("serialize unknown observation");
        assert!(wire.get("active_job_count").is_none());
        assert!(wire.get("live_daemon_job_count").is_none());
        assert_eq!(
            summary.next_action.as_deref(),
            Some("homeboy runner status homeboy-lab --full")
        );
    }

    #[test]
    fn admission_summary_busy_is_not_safe_to_rotate_but_still_accepting() {
        let mut report = base_report();
        report.active_job_state = RunnerActiveJobState::Available;
        report.daemon_freshness = Some(fresh_daemon_freshness());
        report.active_job_count = 3;
        let summary = report.admission_summary();
        assert!(
            summary.accepting_jobs,
            "a busy fresh runner still admits work"
        );
        assert_eq!(summary.active_job_count, Some(3));
        assert!(
            !summary.safe_to_rotate,
            "an authoritative active job blocks rotation"
        );
    }

    #[test]
    fn admission_summary_build_drift_still_requires_lease_evidence() {
        let mut report = base_report();
        report.active_job_state = RunnerActiveJobState::Available;
        report.stale_daemon = Some(RunnerStaleDaemonWarning {
            severity: "error",
            verification: RunnerDaemonVerification::Compared,
            probe_error: None,
            mismatch_predicate: "active_daemon_control_plane_version != job_command_binary_version",
            session_homeboy_version: "0.299.0".to_string(),
            current_homeboy_version: "0.299.2".to_string(),
            session_homeboy_build_identity: None,
            current_homeboy_build_identity: None,
            controller_homeboy_version: None,
            controller_homeboy_build_identity: None,
            compatibility_reason: None,
            active_daemon_control_plane_version: "0.299.0".to_string(),
            job_command_binary_version: "0.299.0".to_string(),
            active_daemon_control_plane_build_identity: None,
            job_command_binary_build_identity: None,
            refresh_command:
                "homeboy runner refresh-homeboy homeboy-lab --ref v0.299.2 --reconnect".to_string(),
            stale_runtime_paths: Vec::new(),
            changed_runtime_paths: Vec::new(),
            message: "stale".to_string(),
            recovery_commands: Vec::new(),
            recovery_actions: Vec::new(),
        });
        let summary = report.admission_summary();
        assert!(
            !summary.daemon_fresh,
            "missing lease evidence cannot be reported as fresh"
        );
        assert!(summary.daemon_compatible);
        assert!(
            !summary.accepting_jobs,
            "an incompatible daemon must not admit work"
        );
        assert_eq!(
            summary.next_action.as_deref(),
            Some("homeboy runner status homeboy-lab --full"),
            "missing proof offers inspection, not mutation"
        );
        assert!(!summary.safe_to_rotate, "missing daemon evidence is unsafe");
    }

    #[test]
    fn admission_summary_missing_freshness_fails_closed_with_diagnostic_action() {
        let mut report = base_report();
        report.active_job_state = RunnerActiveJobState::Available;

        let summary = report.admission_summary();

        assert!(!summary.daemon_fresh);
        assert!(summary.daemon_compatible);
        assert!(!summary.accepting_jobs);
        assert!(!report.daemon_fresh_for_admission());
        assert_eq!(
            summary.next_action.as_deref(),
            Some("homeboy runner status homeboy-lab --full")
        );
    }

    #[test]
    fn persisted_session_drift_recommends_an_accepted_release_tag() {
        let warning = RunnerStaleDaemonWarning::new(
            "homeboy-lab",
            "0.323.1".to_string(),
            "0.323.1".to_string(),
            Some("homeboy 0.323.1+c8a6673b6abc".to_string()),
            Some("homeboy 0.323.1+c8a6673b6abc".to_string()),
        )
        .with_persisted_session_version("homeboy-lab", "0.323.0".to_string());
        let commands = warning.safe_recovery_commands();
        assert_eq!(commands.len(), 1);
        let args: Vec<_> = commands[0].split_whitespace().collect();
        let target = args.windows(2).find(|args| args[0] == "--ref").unwrap()[1];
        assert!(crate::homeboy_refresh::release_version(target).is_ok());
        assert_eq!(target, crate::homeboy_refresh::controller_refresh_ref());
    }

    #[test]
    fn admission_summary_disconnected_points_to_connect() {
        let mut report = base_report();
        report.connected = false;
        report.state = RunnerSessionState::Disconnected;
        let summary = report.admission_summary();
        assert!(!summary.connected);
        assert!(!summary.accepting_jobs);
        assert!(
            summary
                .next_action
                .as_deref()
                .is_some_and(|a| a.contains("connect")),
            "disconnected points at connect: {:?}",
            summary.next_action
        );
    }

    #[test]
    fn admission_summary_lease_missing_with_unavailable_reconciliation_plan_is_terminal() {
        let mut report = base_report();
        report.connected = false;
        report.state = RunnerSessionState::Disconnected;
        report.active_job_state = RunnerActiveJobState::Available;
        report.daemon_freshness = Some(DaemonFreshnessReport {
            fresh: false,
            stale_reason_code: Some(homeboy_core::daemon::DaemonStaleReasonCode::LeaseMissing),
            restartable: false,
            lease_id: None,
            pid: None,
            recovery_evidence: Some(homeboy_core::daemon::DaemonRecoveryEvidence::Unavailable),
            ownership_evidence: Some("ambiguous foreground daemon candidates".to_string()),
            adoption_command: None,
            binary_hash: None,
            daemon_version: None,
            daemon_build_identity: None,
            runtime_paths: None,
            active_jobs: 0,
            termination_evidence: None,
            repair_plan: vec![homeboy_core::daemon::DaemonRepairStep::text(
                "runner_reconcile_leaseless_orphans",
                "homeboy runner connect homeboy-lab --reconcile-leaseless-orphans",
            )],
        });

        let summary = report.admission_summary();

        assert!(!summary.daemon_fresh);
        assert!(!summary.accepting_jobs);
        assert!(!summary.safe_to_rotate);
        assert_eq!(summary.next_action, None);
    }

    #[test]
    fn admission_summary_known_stale_version_is_safe_to_rotate_when_idle() {
        let mut report = base_report();
        report.active_job_state = RunnerActiveJobState::Available;
        report.daemon_freshness = Some(DaemonFreshnessReport {
            fresh: false,
            stale_reason_code: Some(homeboy_core::daemon::DaemonStaleReasonCode::VersionMismatch),
            restartable: false,
            lease_id: Some("known-lease".to_string()),
            pid: Some(1234),
            recovery_evidence: None,
            ownership_evidence: Some("reachable daemon lease and PID verified".to_string()),
            adoption_command: None,
            binary_hash: None,
            daemon_version: Some("0.1.0".to_string()),
            daemon_build_identity: None,
            runtime_paths: None,
            active_jobs: 0,
            termination_evidence: None,
            repair_plan: Vec::new(),
        });

        let summary = report.admission_summary();

        assert!(!summary.daemon_fresh);
        assert!(!summary.accepting_jobs);
        assert!(summary.safe_to_rotate);
    }

    /// #15995: a stale daemon still running jobs cannot rotate, so its next
    /// action must not be the refresh that refuses until those jobs drain.
    #[test]
    fn admission_summary_busy_stale_daemon_points_to_its_jobs_not_a_refused_refresh() {
        let mut report = base_report();
        report.active_job_state = RunnerActiveJobState::Available;
        report.active_job_count = 1;
        report.stale_daemon = Some(RunnerStaleDaemonWarning::new(
            "homeboy-lab",
            "0.428.0".to_string(),
            "0.428.2".to_string(),
            Some("homeboy 0.428.0+19c5f4e54f8e95cd31dd1dcb036e885770c36344".to_string()),
            Some("homeboy 0.428.2+5159b413bc0e0e46be3642fac202f4e0866d93b1".to_string()),
        ));
        report.daemon_freshness = Some(DaemonFreshnessReport {
            fresh: false,
            stale_reason_code: Some(homeboy_core::daemon::DaemonStaleReasonCode::VersionMismatch),
            restartable: false,
            lease_id: Some("known-lease".to_string()),
            pid: Some(1234),
            recovery_evidence: Some(homeboy_core::daemon::DaemonRecoveryEvidence::Recoverable),
            ownership_evidence: Some("reachable daemon lease and PID verified".to_string()),
            adoption_command: None,
            binary_hash: None,
            daemon_version: Some("0.428.0".to_string()),
            daemon_build_identity: Some(
                "homeboy 0.428.0+19c5f4e54f8e95cd31dd1dcb036e885770c36344".to_string(),
            ),
            runtime_paths: None,
            active_jobs: 1,
            termination_evidence: None,
            repair_plan: vec![crate::daemon_repair::action_step(
                crate::daemon_repair::RUNNER_REFRESH_HOMEBOY,
                crate::daemon_repair::refresh_homeboy_action_for_ref(
                    "homeboy-lab",
                    Some("v0.428.2"),
                ),
            )],
        });

        let summary = report.admission_summary();
        assert!(!summary.safe_to_rotate);
        assert_eq!(summary.active_job_count, Some(1));
        assert_eq!(
            summary.next_action.as_deref(),
            Some("homeboy runner job list homeboy-lab")
        );

        report.active_job_count = 0;
        report.daemon_freshness.as_mut().unwrap().active_jobs = 0;
        let idle = report.admission_summary();
        assert!(idle.safe_to_rotate);
        assert_eq!(
            idle.next_action.as_deref(),
            Some("homeboy runner refresh-homeboy homeboy-lab --ref v0.428.2 --reconnect"),
            "once drained, the refresh is the next action again"
        );
    }

    #[test]
    fn admission_summary_known_configured_drift_recovers_an_idle_daemon() {
        let mut report = base_report();
        report.active_job_state = RunnerActiveJobState::Available;
        report.stale_daemon = Some(RunnerStaleDaemonWarning::new(
            "homeboy-lab",
            "0.370.1".to_string(),
            "0.371.0".to_string(),
            Some("homeboy 0.370.1+0732665ce13b8a70cbf8915b1c9c1ddc0fd5eb09".to_string()),
            Some("homeboy 0.371.0+d1a1a6d2092f250780ccf40a57a12becff2a164f".to_string()),
        ));
        report.daemon_freshness = Some(DaemonFreshnessReport {
            fresh: false,
            stale_reason_code: Some(homeboy_core::daemon::DaemonStaleReasonCode::VersionMismatch),
            restartable: false,
            lease_id: Some("known-lease".to_string()),
            pid: Some(1234),
            recovery_evidence: Some(homeboy_core::daemon::DaemonRecoveryEvidence::Recoverable),
            ownership_evidence: Some("reachable daemon lease and PID verified".to_string()),
            adoption_command: None,
            binary_hash: None,
            daemon_version: Some("0.370.1".to_string()),
            daemon_build_identity: Some(
                "homeboy 0.370.1+0732665ce13b8a70cbf8915b1c9c1ddc0fd5eb09".to_string(),
            ),
            runtime_paths: None,
            active_jobs: 0,
            termination_evidence: None,
            repair_plan: vec![crate::daemon_repair::action_step(
                crate::daemon_repair::RUNNER_REFRESH_HOMEBOY,
                crate::daemon_repair::refresh_homeboy_action_for_ref(
                    "homeboy-lab",
                    Some("v0.371.0"),
                ),
            )],
        });
        let summary = report.admission_summary();

        assert!(!summary.accepting_jobs);
        assert!(summary.safe_to_rotate);
        assert_eq!(summary.active_job_count, Some(0));
        assert_eq!(
            summary.next_action.as_deref(),
            Some("homeboy runner refresh-homeboy homeboy-lab --ref v0.371.0 --reconnect")
        );
    }

    #[test]
    fn admission_summary_missing_or_unclassified_freshness_is_not_safe_to_rotate() {
        let mut missing_report = base_report();
        missing_report.active_job_state = RunnerActiveJobState::Available;
        assert!(!missing_report.admission_summary().safe_to_rotate);

        let mut unclassified_report = missing_report;
        unclassified_report.daemon_freshness = Some(DaemonFreshnessReport {
            fresh: false,
            stale_reason_code: None,
            ..fresh_daemon_freshness()
        });
        assert!(!unclassified_report.admission_summary().safe_to_rotate);
    }

    #[test]
    fn admission_summary_incompatible_daemon_with_ambiguous_rotation_evidence_is_not_safe_to_rotate(
    ) {
        let mut report = base_report();
        report.active_job_state = RunnerActiveJobState::Available;
        report.stale_daemon = Some(RunnerStaleDaemonWarning::new(
            "homeboy-lab",
            "0.374.5".to_string(),
            "0.376.0".to_string(),
            Some("homeboy 0.374.5+stale".to_string()),
            Some("homeboy 0.376.0+current".to_string()),
        ));
        report.daemon_freshness = Some(DaemonFreshnessReport {
            fresh: false,
            stale_reason_code: Some(homeboy_core::daemon::DaemonStaleReasonCode::VersionMismatch),
            restartable: false,
            lease_id: None,
            pid: None,
            recovery_evidence: None,
            ownership_evidence: None,
            adoption_command: None,
            binary_hash: None,
            daemon_version: Some("0.374.5".to_string()),
            daemon_build_identity: None,
            runtime_paths: None,
            active_jobs: 0,
            termination_evidence: None,
            repair_plan: Vec::new(),
        });

        let summary = report.admission_summary();

        assert!(summary.daemon_compatible);
        assert!(
            !summary.safe_to_rotate,
            "ambiguous lease ownership must still fence rotation"
        );
    }

    #[test]
    fn terminal_ownership_suppresses_every_admission_action() {
        let mut report = base_report();
        report.active_job_state = RunnerActiveJobState::Unavailable;
        report.daemon_freshness = Some(DaemonFreshnessReport {
            fresh: false,
            stale_reason_code: Some(homeboy_core::daemon::DaemonStaleReasonCode::LeaseMissing),
            restartable: false,
            lease_id: None,
            pid: None,
            recovery_evidence: Some(homeboy_core::daemon::DaemonRecoveryEvidence::Unavailable),
            ownership_evidence: Some("ambiguous remote daemon candidates".to_string()),
            adoption_command: None,
            binary_hash: None,
            daemon_version: None,
            daemon_build_identity: None,
            runtime_paths: None,
            active_jobs: 0,
            termination_evidence: None,
            repair_plan: Vec::new(),
        });

        let summary = report.admission_summary();

        assert_eq!(summary.next_action, None);
    }

    #[test]
    fn disconnected_runner_points_to_connect() {
        let mut report = base_report();
        report.connected = false;
        report.state = RunnerSessionState::Disconnected;

        let summary = report.admission_summary();

        assert_eq!(
            summary.next_action.as_deref(),
            Some("homeboy runner connect homeboy-lab")
        );
    }
}

/// Whether a stale-daemon report is evidence of an observed mismatch or an
/// admission that nothing could be compared at all.
///
/// A compatibility gate has three answers, not two: fresh, stale, and *not
/// established*. Collapsing the third into either of the other two is how the
/// signal becomes untrustworthy — silently fresh (#11106) or uniformly stale
/// (#11101). The same three-state shape is already used by
/// `runtime_overlay_freshness::RuntimeOverlayBuildStatus`, whose
/// `UnknownProbeFailed` variant exists because a failed probe once certified a
/// stale build as current.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunnerDaemonVerification {
    /// The runner's configured identity was read and compared. Everything in
    /// the warning body is an observation.
    Compared,
    /// A verification path exists for this runner, but the probe that feeds it
    /// failed. Nothing was compared; `probe_error` carries the reason.
    ProbeFailed,
}

impl RunnerDaemonVerification {
    /// Whether this verdict rests on an actual comparison.
    pub fn is_compared(self) -> bool {
        self == Self::Compared
    }
}

/// Reported in the version fields of a warning that compared nothing.
///
/// The previous behaviour substituted the session's own recorded version for
/// the one it failed to read, which guaranteed the version comparison passed
/// and published a fabricated "current version" (#11106). A sentinel that
/// cannot be mistaken for a release is the honest alternative.
pub const UNVERIFIED_HOMEBOY_VERSION: &str = "unverified";

/// Severity of a report that established neither freshness nor staleness.
pub const RUNNER_DAEMON_SEVERITY_UNKNOWN: &str = "unknown";

/// `compatibility_reason` for a runner whose identity probe failed outright.
pub const IDENTITY_PROBE_FAILED_REASON: &str = "identity_probe_failed";

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RunnerStaleDaemonWarning {
    pub severity: &'static str,
    /// Whether this report compared anything at all.
    ///
    /// [`RunnerDaemonVerification::Compared`] for every warning derived from an
    /// observed version/identity comparison. The other two variants mark a
    /// report that is neither "fresh" nor "stale" but *unverified*, and are the
    /// only states for which the version fields are sentinels rather than
    /// observations.
    pub verification: RunnerDaemonVerification,
    /// The verbatim probe failure, when one is what prevented verification.
    ///
    /// Previously discarded by an `unwrap_or_else` that replaced the failure
    /// with the session's own version, so an unreachable runner rendered as a
    /// vague identity warning instead of the real SSH error (#11106).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub probe_error: Option<String>,
    /// The exact machine-field predicate that made this warning reject admission.
    /// The compared raw fields are serialized alongside this predicate.
    pub mismatch_predicate: &'static str,
    pub session_homeboy_version: String,
    pub current_homeboy_version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_homeboy_build_identity: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_homeboy_build_identity: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub controller_homeboy_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub controller_homeboy_build_identity: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compatibility_reason: Option<&'static str>,
    pub active_daemon_control_plane_version: String,
    pub job_command_binary_version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_daemon_control_plane_build_identity: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job_command_binary_build_identity: Option<String>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub refresh_command: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stale_runtime_paths: Vec<RunnerStaleRuntimePath>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changed_runtime_paths: Vec<RunnerChangedRuntimePath>,
    pub message: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recovery_commands: Vec<String>,
    /// Argv for `recovery_commands`, in the same order.
    ///
    /// Deliberately not serialized: the wire contract stays the string list.
    /// This is how an in-process consumer executes a recovery without parsing
    /// that list back into arguments (#11104). It is written only through
    /// [`RunnerStaleDaemonWarning::set_recovery`], which renders the strings
    /// from the argv, so the two can never disagree. It is empty exactly when
    /// the recovery has no typed form.
    #[serde(skip)]
    pub(crate) recovery_actions: Vec<ExecutableAction>,
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
    /// Recovery commands safe to publish to an operator or machine consumer.
    ///
    /// Persisted command text is evidence, not authority. A refresh is exposed
    /// only when its release tag is no older than the configured job binary
    /// and it cannot request a downgrade. Unreleased builds use dev-sync.
    pub fn safe_recovery_commands(&self) -> Vec<String> {
        self.safe_recovery_actions()
            .into_iter()
            .map(|action| action.render_command())
            .collect()
    }

    /// Typed actions authorized by the same monotonic contract as the
    /// published command projection. Callers must not reconstruct actions from
    /// persisted command text.
    pub(crate) fn safe_recovery_actions(&self) -> Vec<ExecutableAction> {
        self.recovery_actions
            .iter()
            .filter_map(|action| {
                if action.safety == ActionSafety::ReadOnly {
                    return Some(action.clone());
                }
                if action.args.get(1).map(String::as_str) != Some("refresh-homeboy") {
                    return None;
                }
                let configured = semver::Version::parse(
                    self.job_command_binary_version
                        .trim()
                        .strip_prefix("homeboy ")
                        .unwrap_or(self.job_command_binary_version.trim()),
                )
                .ok()?;
                let target = action
                    .args
                    .windows(2)
                    .find_map(|args| (args[0] == "--ref").then(|| args[1].as_str()))?;
                (crate::homeboy_refresh::release_version(target).ok()? >= configured
                    && !action.args.iter().any(|arg| arg == "--allow-downgrade"))
                .then(|| action.clone())
            })
            .collect()
    }

    /// Remove persisted recovery text that has not passed the monotonic output
    /// contract before embedding this warning in a broader serialized report.
    pub fn sanitized_for_output(&self) -> Self {
        let mut warning = self.clone();
        let commands = self.safe_recovery_commands();
        warning.refresh_command = commands.join(" && ");
        warning.recovery_commands = commands;
        warning.recovery_actions.clear();
        warning
    }

    pub fn new(
        runner_id: &str,
        session_homeboy_version: String,
        current_homeboy_version: String,
        session_homeboy_build_identity: Option<String>,
        current_homeboy_build_identity: Option<String>,
    ) -> Self {
        let version_matches =
            same_homeboy_version(&session_homeboy_version, &current_homeboy_version);
        let identity_matches = session_homeboy_build_identity
            .as_deref()
            .zip(current_homeboy_build_identity.as_deref())
            .is_some_and(|(session, current)| session.trim() == current.trim());
        let mismatch_predicate = if !version_matches {
            "active_daemon_control_plane_version != job_command_binary_version"
        } else if !identity_matches {
            "active_daemon_control_plane_build_identity != job_command_binary_build_identity"
        } else {
            "daemon_configured_identity == equal"
        };
        let recovery_actions = Vec::new();
        let recovery_commands = rendered_commands(&recovery_actions);
        let message = if !version_matches {
            format!(
                "connected runner daemon control plane version `{session_homeboy_version}` differs from configured job command binary version `{current_homeboy_version}` on runner `{runner_id}`; build drift is informational"
            )
        } else if !identity_matches {
            format!(
                "connected runner daemon control plane build identity `{:?}` differs from configured job command binary build identity `{:?}` on runner `{runner_id}`; build drift is informational",
                session_homeboy_build_identity,
                current_homeboy_build_identity,
            )
        } else {
            "connected runner daemon and configured job command binary have equal immutable identities; a separate admission predicate must identify any rejection".to_string()
        };
        Self {
            severity: "info",
            verification: RunnerDaemonVerification::Compared,
            probe_error: None,
            mismatch_predicate,
            active_daemon_control_plane_version: session_homeboy_version.clone(),
            job_command_binary_version: current_homeboy_version.clone(),
            active_daemon_control_plane_build_identity: session_homeboy_build_identity.clone(),
            job_command_binary_build_identity: current_homeboy_build_identity.clone(),
            session_homeboy_version,
            current_homeboy_version,
            session_homeboy_build_identity,
            current_homeboy_build_identity,
            controller_homeboy_version: None,
            controller_homeboy_build_identity: None,
            compatibility_reason: None,
            refresh_command: recovery_commands.join(" && "),
            message,
            stale_runtime_paths: Vec::new(),
            changed_runtime_paths: Vec::new(),
            recovery_commands,
            recovery_actions,
        }
    }

    /// The honest verdict when reading the runner's configured identity failed.
    ///
    /// The probe error is carried verbatim instead of being swallowed, and the
    /// unread "current" version stays a sentinel rather than a copy of the
    /// session's own version — the substitution that made an unreachable runner
    /// satisfy its own version comparison (#11106).
    pub fn probe_failed(
        runner_id: &str,
        session_homeboy_version: String,
        session_homeboy_build_identity: Option<String>,
        configured_executable: &str,
        probe_error: String,
    ) -> Self {
        let message = format!(
            "runner `{runner_id}` daemon compatibility is UNVERIFIED: reading the configured job command binary `{configured_executable}` failed ({probe_error}); neither a match nor a mismatch was established, so this runner's freshness is unknown rather than proven"
        );
        Self::unverified(
            runner_id,
            RunnerDaemonVerification::ProbeFailed,
            IDENTITY_PROBE_FAILED_REASON,
            session_homeboy_version,
            session_homeboy_build_identity,
            Some(probe_error),
            message,
        )
    }

    /// Shared body of the unverified verdict.
    ///
    /// Nothing here is a comparison: the "current"/"job command binary" fields
    /// hold [`UNVERIFIED_HOMEBOY_VERSION`] so no downstream reader can mistake
    /// them for something that was read off the runner, and the recovery is a
    /// read-only re-probe rather than a mutating refresh — replacing a binary
    /// is not the answer to "I could not look at it".
    fn unverified(
        runner_id: &str,
        verification: RunnerDaemonVerification,
        compatibility_reason: &'static str,
        session_homeboy_version: String,
        session_homeboy_build_identity: Option<String>,
        probe_error: Option<String>,
        message: String,
    ) -> Self {
        let controller = crate::controller_identity::compatibility_controller_identity();
        let mut warning = Self {
            severity: RUNNER_DAEMON_SEVERITY_UNKNOWN,
            verification,
            probe_error,
            mismatch_predicate: match verification {
                RunnerDaemonVerification::ProbeFailed => "daemon_identity_probe == failed",
                _ => "daemon_identity_probe == unavailable",
            },
            active_daemon_control_plane_version: session_homeboy_version.clone(),
            job_command_binary_version: UNVERIFIED_HOMEBOY_VERSION.to_string(),
            active_daemon_control_plane_build_identity: session_homeboy_build_identity.clone(),
            job_command_binary_build_identity: None,
            session_homeboy_version,
            current_homeboy_version: UNVERIFIED_HOMEBOY_VERSION.to_string(),
            session_homeboy_build_identity,
            current_homeboy_build_identity: None,
            controller_homeboy_version: Some(controller.version),
            controller_homeboy_build_identity: Some(controller.display),
            compatibility_reason: Some(compatibility_reason),
            refresh_command: String::new(),
            message,
            stale_runtime_paths: Vec::new(),
            changed_runtime_paths: Vec::new(),
            recovery_commands: Vec::new(),
            recovery_actions: Vec::new(),
        };
        warning.set_recovery(vec![crate::daemon_repair::diagnose_action(runner_id)]);
        warning
    }

    /// `true` when this report compared nothing, so it proves neither freshness
    /// nor staleness.
    pub fn is_unverified(&self) -> bool {
        !self.verification.is_compared()
    }

    /// The one place a recovery is assigned.
    ///
    /// Argv in, text out: `recovery_commands` and `refresh_command` are both
    /// rendered from the actions, so a consumer can execute the recovery
    /// directly and still read exactly what it would run.
    fn set_recovery(&mut self, actions: Vec<ExecutableAction>) {
        self.recovery_commands = rendered_commands(&actions);
        self.refresh_command = self.recovery_commands.join(" && ");
        self.recovery_actions = actions;
    }

    pub fn with_identity_unverifiable(
        mut self,
        runner_id: &str,
        configured_executable: &str,
        unverifiable: bool,
    ) -> Self {
        if unverifiable {
            self.mismatch_predicate = "immutable_build_identity(active_daemon_control_plane_build_identity) && immutable_build_identity(job_command_binary_build_identity) == false";
            self.message = format!(
                "connected runner daemon build identity could not be verified against configured executable `{configured_executable}`; run `{} self identity` on the runner and ensure it reports `git_commit` or an exact `display`, then run recovery_commands if needed",
                shell::quote_arg(configured_executable),
            );
            self.set_recovery(recovery_action(runner_id));
        }
        self
    }

    /// A persisted session record participates in readiness independently of
    /// the live daemon endpoint. Report it separately so equal live identities
    /// cannot hide a stale persisted generation.
    pub fn with_persisted_session_version(
        mut self,
        runner_id: &str,
        persisted_session_version: String,
    ) -> Self {
        if !same_homeboy_version(&persisted_session_version, &self.current_homeboy_version)
            && self.mismatch_predicate == "daemon_configured_identity == equal"
        {
            self.mismatch_predicate = "session_homeboy_version != job_command_binary_version";
            self.session_homeboy_version = persisted_session_version;
            self.message = format!(
                "persisted runner session version `{}` differs from configured job command binary version `{}`; run recovery_commands in order when runner active jobs are drained",
                self.session_homeboy_version, self.current_homeboy_version,
            );
            self.set_recovery(recovery_action(runner_id));
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
            for path in &mut self.stale_runtime_paths {
                path.path = redact_runtime_value(&path.path);
                path.loaded_fingerprint = redact_runtime_value(&path.loaded_fingerprint);
                path.current_fingerprint = redact_runtime_value(&path.current_fingerprint);
            }
            for path in &mut self.changed_runtime_paths {
                path.loaded_path = path.loaded_path.as_deref().map(redact_runtime_value);
                path.configured_path = path.configured_path.as_deref().map(redact_runtime_value);
            }
            self.mismatch_predicate = "runtime_paths.loaded != runtime_paths.configured";
            let stale_paths = self.stale_runtime_paths.iter().map(|path| {
                format!(
                    "{} at `{}` fingerprint `{}` -> `{}`",
                    path.env, path.path, path.loaded_fingerprint, path.current_fingerprint,
                )
            });
            let changed_paths = self.changed_runtime_paths.iter().map(|path| {
                format!(
                    "{} path `{:?}` -> `{:?}`",
                    path.env, path.loaded_path, path.configured_path,
                )
            });
            self.message = format!(
                "connected runner daemon runtime paths are stale: {}; run recovery_commands after active jobs are drained to replace the active daemon with the configured runtime paths",
                stale_paths.chain(changed_paths).collect::<Vec<_>>().join("; ")
            );
            if self.recovery_actions.is_empty() {
                self.set_recovery(recovery_action(runner_id));
            }
        }
        self
    }
}

fn redact_runtime_value(value: &str) -> String {
    homeboy_core::redaction::RedactionPolicy::default().redact_env_value(value)
}

/// The recovery for an identity-drifted runner daemon, as argv.
///
/// A rendered string is derived from this; nothing derives argv from a string.
fn recovery_action(runner_id: &str) -> Vec<ExecutableAction> {
    vec![crate::daemon_repair::refresh_homeboy_action_for_ref(
        runner_id,
        Some(&crate::homeboy_refresh::controller_refresh_ref()),
    )]
}

fn rendered_commands(actions: &[ExecutableAction]) -> Vec<String> {
    actions
        .iter()
        .map(ExecutableAction::render_command)
        .collect()
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
    /// The remote daemon could not be authoritatively stopped. Its jobs and
    /// lifecycle remain ambiguous, while the controller session is retained.
    pub partial: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_recovery_command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session: Option<RunnerSession>,
    pub session_path: String,
}
