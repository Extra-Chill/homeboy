//! Shared runner contract: behavior-free types and env-var constants that both
//! `homeboy-core` and the optional `homeboy-runner` feature crate depend on.
//!
//! Runner is an optional Lab-offload feature; core must not depend on runner
//! *behavior*. But some core code legitimately needs to name runner *concepts*
//! (e.g. the runner kind, or the env-var markers used when an exec crosses a
//! remote-runner boundary). Those plain-data contracts live here so core can
//! reference them without a `core -> runner` edge.
//!
//! Two single-type crates were folded in here for the same reason they existed
//! separately — both are behavior-free runner contracts below core, and both
//! were already depended on by a subset of this crate's dependents:
//!
//! - [`Placement`] (was `homeboy-cli-contract`): the requested execution
//!   location, read by core's Lab routing and by the runner's Lab selection.
//! - [`AgentTaskProviderRunnerSource`] (was `homeboy-agents-contract`): a
//!   managed source checkout homeboy keeps synced on the runner, read by core's
//!   agent-runtime manifest and by `homeboy-agents`.

mod execution_placement;
mod placement;
mod provider_source_types;

pub use execution_placement::{
    EffectiveExecutionPlacement, ExecutionPlacementDecision, ExecutionPlacementFallback,
    ExecutionPlacementIdentity, ExecutionPlacementOutcome, ExecutionPlacementOverrideAuthorization,
    ExecutionPlacementRequirement, ExecutionPlacementRunnerSelection, RunnerSelectionSource,
    CONTROLLER_LOCAL_SUBMISSION_POLICY_ID,
};
pub use placement::Placement;
pub use provider_source_types::AgentTaskProviderRunnerSource;

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

/// The kind of runner backing a homeboy runner definition.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunnerKind {
    Local,
    Ssh,
}

/// Which side of a runner exchange owns a lifecycle resource.
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

/// File + byte counts for a workspace sync.
#[derive(Debug, Clone, Copy, Default, Serialize, PartialEq, Eq)]
pub struct ByteFileCounts {
    pub files: usize,
    pub bytes: u64,
}

/// A lease describing a runner's materialized workspace.
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

/// A summary of a runner's current workspace materialization.
#[derive(Debug, Clone, Serialize)]
pub struct RunnerWorkspaceCurrentSummary {
    pub local_path: String,
    pub remote_path: String,
    pub sync_mode: RunnerWorkspaceSyncMode,
    pub materialized: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_commit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_dirty: Option<bool>,
    /// Commit SHA of the synthetic git checkout created for a `snapshot-git`
    /// sync, so write-capable agent-task dispatches can trace the dirty
    /// controller-side worktree back to the synthetic commit that carries it
    /// into the runner workspace. `None` for plain `snapshot`/`git` syncs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub synthetic_checkout_commit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub synthetic_checkout_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub synthetic_checkout_tree: Option<String>,
}

/// A reference to an artifact produced by a runner job. Plain data describing
/// where/how to fetch the artifact; behavior-free so core can name it without a
/// core -> runner edge.
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

/// How a runner workspace is synced before a job runs.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum RunnerWorkspaceSyncMode {
    #[default]
    Snapshot,
    SnapshotGit,
    Git,
}

impl RunnerWorkspaceSyncMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Snapshot => "snapshot",
            Self::SnapshotGit => "snapshot-git",
            Self::Git => "git",
        }
    }
}

/// Options controlling how a runner workspace is synced before a job runs.
#[derive(Debug, Clone, Default)]
pub struct RunnerWorkspaceSyncOptions {
    pub path: String,
    pub mode: RunnerWorkspaceSyncMode,
    pub controller_routed_git: bool,
    pub changed_since_base: Option<String>,
    pub git_fetch_refs: Vec<String>,
    pub snapshot_includes: Vec<String>,
    pub allow_dirty_lab_workspace: bool,
    /// Opaque job-owned token folded into the deterministic remote workspace
    /// path so two distinct executions at the same source HEAD never share a
    /// mutable remote checkout.
    ///
    /// Without this, the git-mode remote path is keyed only on
    /// `(source path, HEAD)`, so a later unrelated job reuses the earlier job's
    /// workspace directory and can observe or delete its state. Every Lab
    /// execution supplies this token; callers that explicitly opt into a stable
    /// identity must reject an already-owned path before materialization.
    pub run_isolation_token: Option<String>,
}

/// Set while a hosted exec runs inside a runner (as opposed to the local host).
pub const RUNNER_HOSTED_EXEC_ENV: &str = "HOMEBOY_RUNNER_HOSTED_EXEC";

/// Private process marker added only while a runner exec crosses a remote
/// runner boundary. Intentionally absent from CLI parsing and argv.
pub const RUNNER_PLACEMENT_RESOLVED_ENV: &str = "HOMEBOY_RUNNER_PLACEMENT_RESOLVED";

/// Identifies the runner an exec is bound to.
pub const RUNNER_ID_ENV: &str = "HOMEBOY_RUNNER_ID";

/// Whether an env-var name is an internal runner control marker (not a
/// user-facing variable). Contract-level classification, so it lives here and
/// core can call it without a core -> runner edge.
pub fn is_internal_control_env(name: &str) -> bool {
    name == RUNNER_PLACEMENT_RESOLVED_ENV
}

/// A tool that must be present on a runner for a capability to be satisfied.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct RunnerRequiredTool {
    id: String,
}

impl RunnerRequiredTool {
    pub fn new(id: impl Into<String>) -> Self {
        Self { id: id.into() }
    }

    pub fn homeboy() -> Self {
        Self::new("homeboy")
    }

    pub fn git() -> Self {
        Self::new("git")
    }

    pub fn id(&self) -> &str {
        &self.id
    }
}

/// A tool + command capability requirement probed on a runner.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunnerToolCapabilityRequirement {
    pub tool: String,
    pub command: String,
    pub env: Vec<String>,
    pub capabilities: Vec<String>,
}

/// Extension-owned command that proves a runner toolchain is usable.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunnerToolchainReadinessProbe {
    pub extension_id: String,
    pub id: String,
    pub program: String,
    pub args: Vec<String>,
    pub repair_command: Option<String>,
    pub diagnostic_env: Vec<String>,
}

/// A resolved set of capability requirements to preflight before running a
/// command on a runner.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunnerCapabilityPreflight {
    pub command: String,
    pub required_tools: Vec<RunnerRequiredTool>,
    pub required_commands: Vec<String>,
    pub required_tool_capabilities: Vec<RunnerToolCapabilityRequirement>,
    pub required_toolchain_probes: Vec<RunnerToolchainReadinessProbe>,
    pub required_components: Vec<String>,
    pub required_env: Vec<String>,
    pub timeout: Option<std::time::Duration>,
}

impl RunnerCapabilityPreflight {
    pub fn is_empty(&self) -> bool {
        self.required_tools.is_empty()
            && self.required_commands.is_empty()
            && self.required_tool_capabilities.is_empty()
            && self.required_toolchain_probes.is_empty()
            && self.required_components.is_empty()
            && self.required_env.is_empty()
    }
}

/// A lab runner capability prepared from a contract, ready to preflight.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedLabRunnerCapability {
    pub command: &'static str,
    pub required_tools: Vec<RunnerRequiredTool>,
    /// Declared workload capabilities retained from the command contract.
    /// Consumers that have a capability inventory can make admission decisions
    /// without reconstructing the original command contract.
    pub required_capabilities: Vec<String>,
}

impl From<PreparedLabRunnerCapability> for RunnerCapabilityPreflight {
    fn from(plan: PreparedLabRunnerCapability) -> Self {
        Self {
            command: plan.command.to_string(),
            required_tools: plan.required_tools,
            required_commands: Vec::new(),
            required_tool_capabilities: Vec::new(),
            required_toolchain_probes: Vec::new(),
            // Capability IDs are opaque to the runner core. Providers and
            // extensions advertise their IDs through the runner inventory.
            required_components: plan.required_capabilities,
            required_env: Vec::new(),
            timeout: None,
        }
    }
}

/// The capability contract a lab runner must satisfy for a command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabRunnerCapabilityContract {
    pub command: &'static str,
    pub required_tools: Vec<RunnerRequiredTool>,
    pub required_capabilities: Vec<String>,
}

/// How a lab runner capability gate is applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LabRunnerGateMode {
    Automatic,
    Explicit,
}

/// The outcome of evaluating a lab runner capability gate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LabRunnerGateDecision {
    Eligible,
    Missing {
        runner_id: String,
        command: &'static str,
        missing_tools: Vec<RunnerRequiredTool>,
        reason: String,
        remediation: Vec<String>,
    },
}

/// Resource-usage metrics captured while a runner child process ran. Pure serde
/// data (no runner behavior) so it can live in the contract and be embedded in
/// core job records (`api_jobs`) without a core -> runner edge.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunnerResourceMetrics {
    pub duration_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_user_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_system_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peak_rss_bytes: Option<u64>,
    pub sample_count: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_process_count_peak: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_guard: Option<RunnerResourceGuardLimits>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guard_violation: Option<RunnerResourceGuardViolation>,
    pub source: String,
}

/// The resource-guard limits in force for a runner child process.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunnerResourceGuardLimits {
    pub rss_limit_bytes: u64,
    pub process_count_limit: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_count_limit_source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_process_count_limit: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_count_limit_ceiling: Option<u64>,
    pub concurrency: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_capacity_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_headroom_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aggregate_rss_budget_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_rss_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aggregate_rss_bytes: Option<u64>,
    pub rss_limit_source: String,
}

/// A resource-guard violation that terminated or flagged a runner child.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunnerResourceGuardViolation {
    pub reason: String,
    pub message: String,
    pub rss_bytes: u64,
    pub rss_limit_bytes: u64,
    pub process_count: u64,
    pub process_count_limit: u64,
}

/// Artifact references produced by a runner mutation (patch, file bundle,
/// operation log). Pure serde data embedded in core job records.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
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

/// How a runner session is tunneled. Pure serde data (with small label helpers)
/// so the core daemon can build/persist sessions without a core -> runner edge.
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

fn default_tunnel_mode() -> RunnerTunnelMode {
    RunnerTunnelMode::DirectSsh
}

/// Which side owns a runner session.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunnerSessionRole {
    Controller,
    Runner,
}

fn default_session_role() -> RunnerSessionRole {
    RunnerSessionRole::Controller
}

/// The connectivity state of a runner session.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunnerSessionState {
    Connected,
    Disconnected,
    Recorded,
}

/// Kernel-derived identity for one local tunnel process instance. This survives
/// controller restart so a recycled PID is never signaled from durable state.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "platform", rename_all = "snake_case")]
pub enum RunnerTunnelProcessStartIdentity {
    Linux {
        starttime_ticks: u64,
    },
    Macos {
        start_seconds: u64,
        start_microseconds: u64,
    },
}

/// A controller-owned reverse forward that exposes a controller-local proxy to
/// a direct SSH runner. The URL is safe to pass to a runner process: it points
/// at the runner loopback listener and never carries controller credentials.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunnerProxyForward {
    pub runner_url: String,
    pub tunnel_pid: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tunnel_process_start_identity: Option<RunnerTunnelProcessStartIdentity>,
}

/// A persisted runner session record. Pure serde data so the core daemon's
/// `/runner/sessions` endpoints can build and persist sessions without a
/// core -> runner edge. `leaseless_recovery_evidence` is carried as opaque JSON
/// (the runner layer owns its typed `RunnerLeaselessRecoveryEvidence`); the
/// daemon never populates it, so the JSON roundtrips identically.
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tunnel_process_start_identity: Option<RunnerTunnelProcessStartIdentity>,
    /// Optional controller proxy exposure owned with this direct SSH session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy_forward: Option<RunnerProxyForward>,
    pub remote_daemon_pid: Option<u32>,
    #[serde(default)]
    pub remote_daemon_lease_id: Option<String>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub leaseless_recovery_evidence: Option<serde_json::Value>,
}

impl RunnerSession {
    /// Which side owns this session's lifecycle, derived from its role.
    pub fn lifecycle_owner(&self) -> RunnerLifecycleOwner {
        match self.role {
            RunnerSessionRole::Controller => RunnerLifecycleOwner::Controller,
            RunnerSessionRole::Runner => RunnerLifecycleOwner::Runner,
        }
    }
}

/// A versioned Lab contract required by a controller or advertised by an
/// executing runner/daemon. Versions are explicit compatibility declarations,
/// not inferred from a Homeboy semver or commit relationship.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct LabCapabilityVersion {
    pub id: String,
    pub version: u32,
}

/// The complete set of Lab contracts every current handoff requires.
///
/// Keep these names protocol-focused. Product-specific capabilities belong in
/// the caller's own contract, not in the generic Lab admission handshake.
pub fn required_lab_handoff_capabilities() -> Vec<LabCapabilityVersion> {
    [
        "daemon-protocol",
        "workspace-materialization",
        "lifecycle-gate-execution",
        "result-schema",
        "artifact-transport",
    ]
    .into_iter()
    .map(|id| LabCapabilityVersion {
        id: id.to_string(),
        version: 1,
    })
    .collect()
}

/// Capability id for the lease-less daemon orphan reconciliation contract.
pub const DAEMON_RECOVERY_LEASELESS_CAPABILITY: &str = "daemon-recovery-leaseless";

/// Capability id for the exact state-loss daemon recovery contract.
pub const DAEMON_RECOVERY_STATE_LOSS_CAPABILITY: &str = "daemon-recovery-state-loss";

/// Capability id for `daemon ensure-running --replacement-operation-id`.
pub const DAEMON_ENSURE_RUNNING_OPERATION_ID_CAPABILITY: &str =
    "daemon-ensure-running-operation-id";

/// The conditionally-needed daemon recovery contracts a runner may advertise.
///
/// These are NOT part of [`required_lab_handoff_capabilities`]: they are
/// recovery contracts negotiated only when a controller must repair a remote
/// daemon, not admission requirements every ordinary handoff needs. A runner
/// that omits them can still execute work; controllers that need the recovery
/// path fall back to scraping the long-option help text when the typed list is
/// absent. Keep these names protocol-focused.
pub fn daemon_recovery_capabilities() -> Vec<LabCapabilityVersion> {
    [
        DAEMON_RECOVERY_LEASELESS_CAPABILITY,
        DAEMON_RECOVERY_STATE_LOSS_CAPABILITY,
        DAEMON_ENSURE_RUNNING_OPERATION_ID_CAPABILITY,
    ]
    .into_iter()
    .map(|id| LabCapabilityVersion {
        id: id.to_string(),
        version: 1,
    })
    .collect()
}

/// The bare long options rendered by a clap help `Options:` heading.
///
/// This is the last-resort contract for daemon-recovery negotiation against a
/// remote that does not advertise the typed [`LabCapabilityVersion`] list. It
/// only accepts an option whose trailing tokens are all value placeholders, so
/// a `/// doc comment` rendering prose after the flag name silently removes it
/// from the advertised contract. Kept here (rather than in the runner crate)
/// so the CLI crate can assert the same predicate against real rendered help.
pub fn declared_long_options(help: &str) -> std::collections::BTreeSet<&str> {
    let mut options = std::collections::BTreeSet::new();
    let mut in_options = false;
    for line in help.lines() {
        let trimmed = line.trim();
        if line == trimmed && trimmed.ends_with(':') {
            in_options = trimmed.eq_ignore_ascii_case("options:");
            continue;
        }
        if !in_options || line == trimmed {
            continue;
        }
        let mut tokens = trimmed.split_whitespace();
        let Some(option) = tokens.next() else {
            continue;
        };
        let option = option.trim_end_matches(',');
        if option.starts_with("--") && tokens.all(is_option_value_placeholder) {
            options.insert(option);
        }
    }
    options
}

/// Whether a clap help token is a value placeholder (`<NAME>` or `[<NAME>]`).
pub fn is_option_value_placeholder(token: &str) -> bool {
    (token.starts_with('<') && token.ends_with('>'))
        || (token.starts_with("[<") && token.ends_with(">]"))
}

/// Decide whether a remote daemon advertises a recovery capability.
///
/// `advertised` is the typed capability list parsed from the remote's
/// self-identity report. `None` means the remote is an older binary that never
/// advertised the field, so the caller's `scrape` (long-option help text) is
/// consulted. `Some(list)` is authoritative: the scrape is never executed.
///
/// The three daemon-recovery negotiations each call this with their own
/// capability id and their own scrape closure, so the decision is defined once
/// instead of copied at every site.
pub fn daemon_recovery_capability_negotiated<Scrape>(
    advertised: Option<&[LabCapabilityVersion]>,
    capability_id: &str,
    scrape: Scrape,
) -> std::result::Result<bool, String>
where
    Scrape: FnOnce() -> std::result::Result<bool, String>,
{
    match advertised {
        Some(capabilities) => Ok(capabilities
            .iter()
            .any(|capability| capability.id == capability_id)),
        None => scrape(),
    }
}

/// Immutable runtime provenance supplied by a controller, runner command, or
/// active daemon. Dirty or incomplete provenance is never eligible for a
/// compatibility admission.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LabRuntimeIdentity {
    pub build_identity: String,
    pub source_revision: String,
    pub clean: bool,
}

/// The independently verified relationship between a requested controller
/// source revision and the executing runner source revision.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LabRuntimeAncestry {
    ExactSource,
    VerifiedNewerDescendant,
    Older,
    Diverged,
    Unknown,
}

/// The controller requirement and runner/daemon evidence that participate in a
/// single Lab handoff admission decision.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LabCapabilityHandshake {
    pub controller: LabRuntimeIdentity,
    pub required_capabilities: Vec<LabCapabilityVersion>,
    pub runner_command: LabRuntimeIdentity,
    pub runner_command_capabilities: Vec<LabCapabilityVersion>,
    pub daemon: LabRuntimeIdentity,
    pub daemon_capabilities: Vec<LabCapabilityVersion>,
    pub ancestry: LabRuntimeAncestry,
}

/// Serialized record of a Lab capability negotiation. Persist this unchanged
/// from preflight through reservation and execution so all phases use one
/// decision rather than recomputing against drifting runtime state.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LabCapabilityNegotiationProvenance {
    pub schema: String,
    pub controller_requirement: LabRuntimeIdentity,
    pub executed_runner_command: LabRuntimeIdentity,
    pub active_daemon: LabRuntimeIdentity,
    pub ancestry: LabRuntimeAncestry,
    pub negotiated_capabilities: Vec<LabCapabilityVersion>,
    pub compatible: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rejection_reason: Option<String>,
}

/// Result of a fail-closed Lab capability admission decision.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LabCapabilityAdmission {
    pub compatible: bool,
    pub provenance: LabCapabilityNegotiationProvenance,
}

/// Evaluate a controller-to-Lab handoff using only explicit compatibility and
/// provenance evidence. A newer source revision is accepted only when its
/// ancestry has already been verified and both the executing command and active
/// daemon advertise every required capability version.
pub fn negotiate_lab_capability_handshake(
    handshake: &LabCapabilityHandshake,
) -> LabCapabilityAdmission {
    let mut rejection_reason = runtime_identity_reason("controller", &handshake.controller)
        .or_else(|| runtime_identity_reason("runner command", &handshake.runner_command))
        .or_else(|| runtime_identity_reason("active daemon", &handshake.daemon));

    if rejection_reason.is_none()
        && handshake.runner_command.source_revision != handshake.daemon.source_revision
    {
        rejection_reason =
            Some("runner command and active daemon source revisions differ".to_string());
    }

    if rejection_reason.is_none()
        && !matches!(
            handshake.ancestry,
            LabRuntimeAncestry::ExactSource | LabRuntimeAncestry::VerifiedNewerDescendant
        )
    {
        rejection_reason = Some(format!(
            "runner provenance is not an exact source or verified newer descendant ({:?})",
            handshake.ancestry
        ));
    }

    if rejection_reason.is_none()
        && handshake.ancestry == LabRuntimeAncestry::ExactSource
        && handshake.controller.source_revision != handshake.runner_command.source_revision
    {
        rejection_reason =
            Some("exact-source ancestry does not match the controller source revision".to_string());
    }

    let required = capability_versions(&handshake.required_capabilities);
    if rejection_reason.is_none()
        && (handshake
            .required_capabilities
            .iter()
            .any(|capability| capability.id.trim().is_empty())
            || required.len()
                != handshake
                    .required_capabilities
                    .iter()
                    .map(|capability| capability.id.as_str())
                    .collect::<BTreeSet<_>>()
                    .len())
    {
        rejection_reason =
            Some("controller capability requirements are incomplete or contradictory".to_string());
    }
    let command = advertised_capability_versions(&handshake.runner_command_capabilities);
    let daemon = advertised_capability_versions(&handshake.daemon_capabilities);
    let negotiated_capabilities = required
        .iter()
        .filter(|&(id, version)| {
            command
                .get(id)
                .is_some_and(|versions| versions.contains(version))
                && daemon
                    .get(id)
                    .is_some_and(|versions| versions.contains(version))
        })
        .map(|(id, version)| LabCapabilityVersion {
            id: id.clone(),
            version: *version,
        })
        .collect();

    if rejection_reason.is_none() {
        for (id, version) in &required {
            if !command
                .get(id)
                .is_some_and(|versions| versions.contains(version))
                || !daemon
                    .get(id)
                    .is_some_and(|versions| versions.contains(version))
            {
                rejection_reason = Some(format!(
                    "required capability `{id}` version {version} is not advertised by both runner command and active daemon"
                ));
                break;
            }
        }
    }

    let compatible = rejection_reason.is_none();
    LabCapabilityAdmission {
        compatible,
        provenance: LabCapabilityNegotiationProvenance {
            schema: "homeboy/lab-capability-negotiation/v1".to_string(),
            controller_requirement: handshake.controller.clone(),
            executed_runner_command: handshake.runner_command.clone(),
            active_daemon: handshake.daemon.clone(),
            ancestry: handshake.ancestry,
            negotiated_capabilities,
            compatible,
            rejection_reason,
        },
    }
}

fn runtime_identity_reason(role: &str, identity: &LabRuntimeIdentity) -> Option<String> {
    if !identity.clean {
        return Some(format!("{role} runtime provenance is dirty"));
    }
    if identity.build_identity.trim().is_empty() || identity.source_revision.trim().is_empty() {
        return Some(format!("{role} runtime provenance is incomplete"));
    }
    None
}

fn capability_versions(capabilities: &[LabCapabilityVersion]) -> BTreeMap<String, u32> {
    let mut versions = BTreeMap::new();
    for capability in capabilities {
        // Conflicting duplicate declarations are deliberately absent from the
        // result, so they cannot accidentally satisfy a requirement.
        match versions.get(&capability.id) {
            Some(previous) if *previous != capability.version => {
                versions.remove(&capability.id);
            }
            Some(_) => {}
            None => {
                versions.insert(capability.id.clone(), capability.version);
            }
        }
    }
    versions
}

fn advertised_capability_versions(
    capabilities: &[LabCapabilityVersion],
) -> BTreeMap<String, BTreeSet<u32>> {
    let mut versions = BTreeMap::new();
    for capability in capabilities {
        versions
            .entry(capability.id.clone())
            .or_insert_with(BTreeSet::new)
            .insert(capability.version);
    }
    versions
}

#[cfg(test)]
mod capability_handshake_tests {
    use super::*;

    fn identity(build: &str, source: &str) -> LabRuntimeIdentity {
        LabRuntimeIdentity {
            build_identity: build.to_string(),
            source_revision: source.to_string(),
            clean: true,
        }
    }

    fn handshake(ancestry: LabRuntimeAncestry) -> LabCapabilityHandshake {
        let capabilities = required_lab_handoff_capabilities();
        LabCapabilityHandshake {
            controller: identity("homeboy 1.0.0+controller", "a"),
            required_capabilities: capabilities.clone(),
            runner_command: identity("homeboy 1.0.1+runner", "b"),
            runner_command_capabilities: capabilities.clone(),
            daemon: identity("homeboy 1.0.1+daemon", "b"),
            daemon_capabilities: capabilities,
            ancestry,
        }
    }

    #[test]
    fn admits_controller_n_with_verified_runner_n_plus_one_and_records_provenance() {
        let admission = negotiate_lab_capability_handshake(&handshake(
            LabRuntimeAncestry::VerifiedNewerDescendant,
        ));
        assert!(admission.compatible);
        assert_eq!(
            admission.provenance.schema,
            "homeboy/lab-capability-negotiation/v1"
        );
        assert_eq!(admission.provenance.negotiated_capabilities.len(), 5);
    }

    #[test]
    fn rejects_controller_n_plus_one_with_older_runner() {
        let admission = negotiate_lab_capability_handshake(&handshake(LabRuntimeAncestry::Older));
        assert!(!admission.compatible);
    }

    #[test]
    fn admits_cross_platform_same_source_builds() {
        let mut handshake = handshake(LabRuntimeAncestry::ExactSource);
        handshake.runner_command = identity("homeboy 1.0.0+linux", "a");
        handshake.daemon = identity("homeboy 1.0.0+macos", "a");
        assert!(negotiate_lab_capability_handshake(&handshake).compatible);
    }

    #[test]
    fn rejects_diverged_unknown_and_dirty_provenance() {
        for ancestry in [LabRuntimeAncestry::Diverged, LabRuntimeAncestry::Unknown] {
            assert!(!negotiate_lab_capability_handshake(&handshake(ancestry)).compatible);
        }
        let mut dirty = handshake(LabRuntimeAncestry::VerifiedNewerDescendant);
        dirty.daemon.clean = false;
        assert!(!negotiate_lab_capability_handshake(&dirty).compatible);
    }

    #[test]
    fn rejects_capability_removal_schema_incompatibility_and_command_daemon_divergence() {
        let mut removed = handshake(LabRuntimeAncestry::VerifiedNewerDescendant);
        removed.daemon_capabilities.pop();
        assert!(!negotiate_lab_capability_handshake(&removed).compatible);

        let mut incompatible = handshake(LabRuntimeAncestry::VerifiedNewerDescendant);
        incompatible.runner_command_capabilities[3].version = 2;
        assert!(!negotiate_lab_capability_handshake(&incompatible).compatible);

        let mut divergent = handshake(LabRuntimeAncestry::VerifiedNewerDescendant);
        divergent.daemon.source_revision = "c".to_string();
        assert!(!negotiate_lab_capability_handshake(&divergent).compatible);
    }

    #[test]
    fn accepts_a_newer_capability_that_explicitly_advertises_the_required_version() {
        let mut handshake = handshake(LabRuntimeAncestry::VerifiedNewerDescendant);
        handshake
            .runner_command_capabilities
            .push(LabCapabilityVersion {
                id: "result-schema".to_string(),
                version: 2,
            });
        handshake.daemon_capabilities.push(LabCapabilityVersion {
            id: "result-schema".to_string(),
            version: 2,
        });
        assert!(negotiate_lab_capability_handshake(&handshake).compatible);
    }
}

#[cfg(test)]
mod daemon_recovery_capability_tests {
    use super::*;

    #[test]
    fn recovery_capabilities_are_their_own_protocol_focused_list() {
        let recovery = daemon_recovery_capabilities();
        let ids: BTreeSet<_> = recovery
            .iter()
            .map(|capability| capability.id.as_str())
            .collect();
        assert_eq!(
            ids,
            BTreeSet::from([
                DAEMON_RECOVERY_LEASELESS_CAPABILITY,
                DAEMON_RECOVERY_STATE_LOSS_CAPABILITY,
                DAEMON_ENSURE_RUNNING_OPERATION_ID_CAPABILITY,
            ])
        );
        assert!(recovery.iter().all(|capability| capability.version == 1));

        let handoff_capabilities = required_lab_handoff_capabilities();
        let handoff: BTreeSet<_> = handoff_capabilities
            .iter()
            .map(|capability| capability.id.as_str())
            .collect();
        assert!(
            ids.is_disjoint(&handoff),
            "recovery capabilities must not leak into the admission handshake"
        );
    }

    #[test]
    fn declared_long_options_accepts_only_bare_value_placeholders() {
        let options = declared_long_options(
            "OPTIONS:\n    --reconcile-leaseless-orphans\n    --confirm-no-daemon-owner\n    --replacement-operation-id <ID>\n    --addr <ADDR>\n",
        );
        assert!(options.contains("--confirm-no-daemon-owner"));
        assert!(options.contains("--replacement-operation-id"));
        assert!(options.contains("--addr"));

        let prose = declared_long_options(
            "Options:\n    --confirm-no-daemon-owner after inspection\n",
        );
        assert!(!prose.contains("--confirm-no-daemon-owner"));
        assert!(prose.is_empty());

        let examples = declared_long_options(
            "Examples:\n    --confirm-no-daemon-owner\n",
        );
        assert!(examples.is_empty());

        let lowercase = declared_long_options("options:\n    --confirm-no-daemon-owner\n");
        assert!(lowercase.contains("--confirm-no-daemon-owner"));
    }

    #[test]
    fn is_option_value_placeholder_matches_only_value_shaped_tokens() {
        assert!(is_option_value_placeholder("<ID>"));
        assert!(is_option_value_placeholder("[<ID>]"));
        assert!(!is_option_value_placeholder("ID"));
        assert!(!is_option_value_placeholder("after inspection"));
    }

    #[test]
    fn typed_capability_advertisement_is_authoritative_and_skips_the_scrape() {
        let advertised: [LabCapabilityVersion; 1] = [LabCapabilityVersion {
            id: DAEMON_RECOVERY_LEASELESS_CAPABILITY.to_string(),
            version: 1,
        }];
        let mut scraped = false;
        let negotiated = daemon_recovery_capability_negotiated(
            Some(advertised.as_slice()),
            DAEMON_RECOVERY_LEASELESS_CAPABILITY,
            || {
                scraped = true;
                Ok(false)
            },
        );
        assert_eq!(negotiated, Ok(true));
        assert!(!scraped, "the typed list must replace the help scrape");
    }

    #[test]
    fn typed_advertisement_missing_the_capability_is_an_authoritative_no() {
        let advertised: [LabCapabilityVersion; 1] = [LabCapabilityVersion {
            id: DAEMON_RECOVERY_STATE_LOSS_CAPABILITY.to_string(),
            version: 1,
        }];
        let mut scraped = false;
        let negotiated = daemon_recovery_capability_negotiated(
            Some(advertised.as_slice()),
            DAEMON_RECOVERY_LEASELESS_CAPABILITY,
            || {
                scraped = true;
                Ok(true)
            },
        );
        assert_eq!(negotiated, Ok(false));
        assert!(!scraped, "a typed no must not be overridden by the scrape");
    }

    #[test]
    fn absent_advertisement_falls_back_to_the_scrape() {
        let mut scraped = false;
        let negotiated = daemon_recovery_capability_negotiated::<_>(
            None,
            DAEMON_RECOVERY_LEASELESS_CAPABILITY,
            || {
                scraped = true;
                Ok(true)
            },
        );
        assert_eq!(negotiated, Ok(true));
        assert!(scraped, "an older binary must fall back to the help scrape");
    }
}
