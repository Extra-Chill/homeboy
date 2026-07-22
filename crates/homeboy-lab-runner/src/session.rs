use serde::ser::{SerializeStruct, Serializer};
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

#[derive(Debug, Clone)]
pub struct RunnerStatusReport {
    pub runner_id: String,
    pub connected: bool,
    pub state: RunnerSessionState,
    pub session: Option<RunnerSession>,
    pub stale_daemon: Option<RunnerStaleDaemonWarning>,
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

#[derive(Debug, Clone, Serialize)]
pub struct RunnerDaemonGenerationStatus {
    pub generation: String,
    pub admission_owner: bool,
    pub drain_state: crate::RollingDrainState,
    /// Persisted admission counter. It is only authoritative when
    /// `observed_active_job_count` is present.
    pub active_job_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observed_active_job_count: Option<usize>,
    pub active_job_count_authoritative: bool,
    pub job_owner_count: usize,
    pub run_owner_count: usize,
    pub artifact_owner_count: usize,
    pub homeboy_build_identity: Option<String>,
    pub remote_daemon_lease_id: Option<String>,
    pub remote_daemon_address: Option<String>,
    pub local_url: Option<String>,
}

impl Serialize for RunnerStatusReport {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let generations =
            crate::generation_store::status_projection(&self.runner_id, self.session.as_ref())
                .unwrap_or_default();
        // Bound the embedded generation view: emit a compact summary
        // (authoritative selected daemon + a count-only breakdown of draining
        // generations) rather than expanding every historical per-generation
        // record (#9478/#9522). On a long-lived runner the full ledger runs to
        // thousands of lines and its non-authoritative per-generation counts
        // make old ownership look like current load. The full inventory remains
        // available through `runner status --generations`, which surfaces the
        // complete `generation_inventory` list. Nothing deserializes this
        // report, so trimming the serialized shape is display-only.
        let generation_summary = RunnerGenerationLedgerSummary::from_projection(&generations);
        let mut state = serializer.serialize_struct("RunnerStatusReport", 17)?;
        state.serialize_field("runner_id", &self.runner_id)?;
        state.serialize_field("connected", &self.connected)?;
        state.serialize_field("state", &self.state)?;
        if let Some(session) = &self.session {
            state.serialize_field("session", session)?;
        }
        state.serialize_field("generations", &generation_summary)?;
        if let Some(stale_daemon) = &self.stale_daemon {
            state.serialize_field("stale_daemon", stale_daemon)?;
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
        state.serialize_field("active_job_count", &self.active_job_count)?;
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

/// A bounded, count-only summary of the daemon generation ledger for status
/// output. Replaces the full per-generation expansion that dominated `runner
/// status` (#9478/#9522): it names the authoritative admission-owner generation
/// and reports how many generations exist and how many are draining, without
/// ever emitting the non-authoritative per-generation `active_job_count` values
/// that made stale ownership look like current load.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RunnerGenerationLedgerSummary {
    /// Total number of tracked daemon generations.
    pub total: usize,
    /// The generation that currently owns admission, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub admission_owner: Option<String>,
    /// Number of generations that are draining (retiring, not accepting new
    /// admission). Reported as a count, never expanded.
    pub draining: usize,
}

impl RunnerGenerationLedgerSummary {
    fn from_projection(generations: &[RunnerDaemonGenerationStatus]) -> Self {
        let admission_owner = generations
            .iter()
            .find(|generation| generation.admission_owner)
            .map(|generation| generation.generation.clone());
        let draining = generations
            .iter()
            .filter(|generation| generation.drain_state == crate::RollingDrainState::Draining)
            .count();
        Self {
            total: generations.len(),
            admission_owner,
            draining,
        }
    }
}

/// One compact, authoritative answer to "is this runner ready for the next
/// workload right now, and is it safe to rotate?" — the lead of `runner
/// status`.
///
/// This is the projection the historical generation ledger was drowning
/// (#9478/#9522): `runner status` embedded every draining daemon generation
/// with full owner counts, and non-authoritative per-generation
/// `active_job_count` values made stale ownership look like current load. This
/// summary is derived only from the authoritative selected-daemon state plus a
/// bounded count of draining generations, so it stays small and fast no matter
/// how much history accumulates. The full generation list remains available
/// behind an explicit detail view.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RunnerAdmissionSummary {
    pub runner_id: String,
    /// Whether the runner's session is connected.
    pub connected: bool,
    /// Whether the selected daemon's build matches what the controller
    /// requires (i.e. no stale-daemon warning). `false` means a version
    /// convergence is needed before admission.
    pub daemon_fresh: bool,
    /// Whether the runner can admit a new workload now: connected, fresh, and
    /// not otherwise blocked.
    pub accepting_jobs: bool,
    /// Authoritative count of jobs the selected daemon is currently running.
    pub active_job_count: usize,
    /// Count of runner jobs whose liveness can no longer be confirmed.
    pub stale_job_count: usize,
    /// The exact build identity of the selected daemon, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub daemon_build_identity: Option<String>,
    /// Number of historical draining generations, summarized rather than
    /// expanded. Never presented as active load.
    pub draining_generation_count: usize,
    /// Whether it is safe to rotate the runner now — true only when no
    /// authoritative current job is active on the selected daemon.
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

    /// Project the authoritative current-state admission summary.
    ///
    /// `draining_generation_count` is supplied by the caller from the same
    /// generation inventory used for the detail view, so the summary and detail
    /// derive from one source. Only the authoritative selected-daemon
    /// `active_job_count` drives `active_job_count`/`safe_to_rotate` — historical
    /// per-generation counts (non-authoritative) never contribute.
    pub fn admission_summary(&self, draining_generation_count: usize) -> RunnerAdmissionSummary {
        let connected = self.is_connected();
        let daemon_fresh = self.stale_daemon.is_none();
        let active_job_count = self.active_job_count;
        let accepting_jobs = connected && daemon_fresh;
        let safe_to_rotate = active_job_count == 0;
        let daemon_build_identity = self
            .session
            .as_ref()
            .and_then(|session| session.homeboy_build_identity.clone());

        let next_action = if !connected {
            Some("homeboy runner connect".to_string())
        } else if !daemon_fresh {
            Some("homeboy runner refresh-homeboy --reconnect".to_string())
        } else if active_job_count > 0 {
            None
        } else {
            None
        };

        RunnerAdmissionSummary {
            runner_id: self.runner_id.clone(),
            connected,
            daemon_fresh,
            accepting_jobs,
            active_job_count,
            stale_job_count: self.stale_runner_job_count,
            daemon_build_identity,
            draining_generation_count,
            safe_to_rotate,
            next_action,
        }
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

#[cfg(test)]
mod status_serialization_tests {
    use super::*;

    #[test]
    fn status_serialization_omits_empty_legacy_fields_and_includes_generations() {
        let report = RunnerStatusReport {
            runner_id: "missing-runner".to_string(),
            connected: false,
            state: RunnerSessionState::Disconnected,
            session: None,
            stale_daemon: None,
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
        // Generations serialize as a bounded count-only summary, not the full
        // per-generation expansion (#9478/#9522). With no generations it is a
        // zero summary.
        assert_eq!(
            value["generations"],
            serde_json::json!({ "total": 0, "draining": 0 })
        );
    }

    #[test]
    fn generation_ledger_summary_counts_without_expanding_or_exposing_active_load() {
        fn gen(
            name: &str,
            admission_owner: bool,
            drain: crate::RollingDrainState,
            noisy_count: usize,
        ) -> RunnerDaemonGenerationStatus {
            RunnerDaemonGenerationStatus {
                generation: name.to_string(),
                admission_owner,
                drain_state: drain,
                // A deliberately large NON-authoritative count: the summary must
                // never surface it as active load (the #9522 confusion).
                active_job_count: noisy_count,
                observed_active_job_count: None,
                active_job_count_authoritative: false,
                job_owner_count: noisy_count,
                run_owner_count: 0,
                artifact_owner_count: 0,
                homeboy_build_identity: None,
                remote_daemon_lease_id: None,
                remote_daemon_address: None,
                local_url: None,
            }
        }

        let projection = vec![
            gen(
                "build-current",
                true,
                crate::RollingDrainState::Admitting,
                0,
            ),
            gen(
                "build-old-1",
                false,
                crate::RollingDrainState::Draining,
                160,
            ),
            gen("build-old-2", false, crate::RollingDrainState::Draining, 82),
        ];

        let summary = RunnerGenerationLedgerSummary::from_projection(&projection);
        assert_eq!(summary.total, 3);
        assert_eq!(summary.admission_owner.as_deref(), Some("build-current"));
        assert_eq!(summary.draining, 2);

        // The serialized summary is a compact object — no per-generation array,
        // no active_job_count fields leaking historical ownership as load.
        let value = serde_json::to_value(&summary).expect("serialize summary");
        assert!(value.get("total").is_some());
        assert!(
            value
                .as_object()
                .is_some_and(|o| !o.contains_key("active_job_count")),
            "summary must never carry a per-generation active_job_count: {value}"
        );
    }

    fn base_report() -> RunnerStatusReport {
        RunnerStatusReport {
            runner_id: "homeboy-lab".to_string(),
            connected: true,
            state: RunnerSessionState::Connected,
            session: None,
            stale_daemon: None,
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

    #[test]
    fn admission_summary_fresh_idle_accepts_and_is_safe_to_rotate() {
        let summary = base_report().admission_summary(0);
        assert!(summary.connected);
        assert!(summary.daemon_fresh);
        assert!(summary.accepting_jobs);
        assert_eq!(summary.active_job_count, 0);
        assert!(summary.safe_to_rotate);
        assert_eq!(summary.next_action, None);
    }

    #[test]
    fn admission_summary_busy_is_not_safe_to_rotate_but_still_accepting() {
        let mut report = base_report();
        report.active_job_count = 3;
        let summary = report.admission_summary(0);
        assert!(
            summary.accepting_jobs,
            "a busy fresh runner still admits work"
        );
        assert_eq!(summary.active_job_count, 3);
        assert!(
            !summary.safe_to_rotate,
            "an authoritative active job blocks rotation"
        );
    }

    #[test]
    fn admission_summary_stale_daemon_refuses_and_points_to_refresh() {
        let mut report = base_report();
        report.stale_daemon = Some(RunnerStaleDaemonWarning {
            severity: "error",
            session_homeboy_version: "0.299.0".to_string(),
            current_homeboy_version: "0.299.2".to_string(),
            session_homeboy_build_identity: None,
            current_homeboy_build_identity: None,
            active_daemon_control_plane_version: "0.299.0".to_string(),
            job_command_binary_version: "0.299.0".to_string(),
            active_daemon_control_plane_build_identity: None,
            job_command_binary_build_identity: None,
            refresh_command: "homeboy runner refresh-homeboy homeboy-lab".to_string(),
            stale_runtime_paths: Vec::new(),
            changed_runtime_paths: Vec::new(),
            message: "stale".to_string(),
            recovery_commands: Vec::new(),
        });
        let summary = report.admission_summary(0);
        assert!(!summary.daemon_fresh);
        assert!(
            !summary.accepting_jobs,
            "a stale daemon must not admit work"
        );
        assert!(
            summary
                .next_action
                .as_deref()
                .is_some_and(|a| a.contains("refresh-homeboy")),
            "stale daemon points at refresh: {:?}",
            summary.next_action
        );
    }

    #[test]
    fn admission_summary_disconnected_points_to_connect() {
        let mut report = base_report();
        report.connected = false;
        report.state = RunnerSessionState::Disconnected;
        let summary = report.admission_summary(0);
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
    fn admission_summary_summarizes_draining_generations_by_count_not_as_load() {
        // The historical draining generations are reported only as a count and
        // never contribute to active_job_count (the #9522 confusion).
        let summary = base_report().admission_summary(7);
        assert_eq!(summary.draining_generation_count, 7);
        assert_eq!(
            summary.active_job_count, 0,
            "draining history is not current load"
        );
        assert!(summary.safe_to_rotate);
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
