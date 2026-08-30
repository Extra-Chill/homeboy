use serde::Serialize;
use serde_json::Value;

use homeboy::core::api_jobs::{Job, JobEvent, JobStatus};
use homeboy::core::EntityCrudOutput;
use homeboy::runner::readonly_probe::ReadOnlyProbeDegradation;
use homeboy::runner::runners::{
    PeerSessionMaintenanceReport, ReverseRunnerWorkerOutput, Runner, RunnerAdmissionSummary,
    RunnerAvailability, RunnerConnectReport, RunnerDaemonGenerationStatus, RunnerDisconnectReport,
    RunnerExecOutput, RunnerStatusReport,
};

use std::collections::BTreeMap;

use super::lifecycle;
use super::refresh_plan;
use super::workspace;
use crate::commands::utils::response::CommandActionableMetadata;

#[derive(Debug, Serialize)]
pub struct RunnerExtra {
    pub variant: &'static str,
    /// Postcondition of a mutating generation reconciliation. This is distinct
    /// from the command envelope: only `converged` restores runner admission.
    /// It intentionally precedes status detail so compact output leads with the
    /// operation's changed state, remaining blocker, and next action.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reconciliation: Option<RunnerReconciliationOutcome>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preferred_lab_runner: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected_lab_runner: Option<LabSelectedRunnerOutput>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub managed_followups: Vec<LabFollowup>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connection: Option<RunnerConnectionOutput>,
    /// Terminal outcome of `runner disconnect`. This makes bounded remote
    /// ambiguity machine-readable without changing other runner commands.
    #[serde(rename = "status", skip_serializing_if = "Option::is_none")]
    pub disconnect_status: Option<RunnerDisconnectStatus>,
    /// The compact authoritative "ready now / safe to rotate" answer. Leads the
    /// status output; the full generation inventory below is detail behind it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub admission_summary: Option<RunnerAdmissionSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inspection: Option<RunnerStatusInspection>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub sessions: Vec<RunnerStatusReport>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub generation_inventory: Vec<RunnerDaemonGenerationStatus>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub operator_hints: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub operator_commands: Vec<RunnerOperatorCommand>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operator_summary: Option<RunnerOperatorSummary>,
    /// Controller-local execution and Lab connection state are separate
    /// contracts. In particular, local placement has no runner connection to
    /// establish.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution_capabilities: Option<RunnerExecutionCapabilities>,
    /// One compact row per configured runner: what exists, whether it is
    /// reachable, and whether it can take work (#9487).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub runner_summaries: Vec<RunnerOperatorSummary>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub operator_summaries: Vec<RunnerOperatorSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub truncation: Option<RunnerTruncation>,
    /// Read-only remote probes that hit their wall-clock bound while composing
    /// this status (#10418). A non-empty list means the status is PARTIAL: the
    /// runner did not answer within the bound, which is the operator's signal
    /// that the Lab is wedged rather than idle.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub probe_degradations: Vec<ReadOnlyProbeDegradation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub peer_session_maintenance: Option<PeerSessionMaintenanceReport>,
}

impl Default for RunnerExtra {
    fn default() -> Self {
        Self {
            variant: "registry",
            preferred_lab_runner: None,
            selected_lab_runner: None,
            managed_followups: Vec::new(),
            connection: None,
            disconnect_status: None,
            reconciliation: None,
            admission_summary: None,
            inspection: None,
            sessions: Vec::new(),
            generation_inventory: Vec::new(),
            operator_hints: Vec::new(),
            operator_commands: Vec::new(),
            operator_summary: None,
            execution_capabilities: None,
            runner_summaries: Vec::new(),
            operator_summaries: Vec::new(),
            truncation: None,
            probe_degradations: Vec::new(),
            peer_session_maintenance: None,
        }
    }
}

/// Bounded postcondition for `runner reconcile`.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct RunnerReconciliationOutcome {
    /// What this invocation changed. `unchanged` means no generation could be
    /// retired from the observed state.
    pub changed_state: String,
    /// State plane that owns direct-runner generation reconciliation.
    pub owner: &'static str,
    /// The one runner and its persisted daemon generations inspected here.
    pub scope: String,
    /// State guaranteed only when `status` is `converged`.
    pub postcondition: &'static str,
    /// `converged`, `partial_progress`, or `blocked`.
    pub status: RunnerReconciliationStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remaining_blocker: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_action: Option<String>,
    /// The observation that must change before retrying this same operation can
    /// make additional progress. This prevents an unbounded self-recommendation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_predicate: Option<String>,
    pub retired_generation_count: usize,
    /// IDs retired by this reconciliation operation under the generation lock.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub retired_generation_ids: Vec<String>,
}

/// Terminality of a `runner reconcile` result.
#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunnerReconciliationStatus {
    Converged,
    PartialProgress,
    Blocked,
}

/// The bounded inspection state for a registry-wide status response.
#[derive(Debug, Serialize)]
pub struct RunnerStatusInspection {
    pub status: &'static str,
    pub partial: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub runner_unavailable: Vec<RunnerStatusUnavailable>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub runner_unqueried: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct RunnerStatusUnavailable {
    pub runner_id: String,
    pub code: String,
    pub message: String,
    pub refresh_command: String,
}

/// Bounded default status contract. Detailed status remains behind `--full`.
#[derive(Debug, Serialize)]
pub struct RunnerOperatorSummary {
    pub identity: String,
    pub state: String,
    pub risk: Vec<String>,
    pub next_action: String,
}

/// Bounded default inventory row for one configured runner.
#[derive(Debug, Serialize)]
pub struct RunnerInventorySummary {
    pub identity: String,
    pub kind: String,
    pub connection_state: String,
    pub admission_state: String,
    pub concurrency: RunnerInventoryConcurrency,
    pub drift: String,
    pub next_action: String,
    pub evidence: RunnerInventoryEvidence,
}

#[derive(Debug, Serialize)]
pub struct RunnerInventoryConcurrency {
    pub active: Option<usize>,
    pub limit: Option<usize>,
}

#[derive(Debug, Serialize)]
pub struct RunnerInventoryEvidence {
    pub environment_ref: String,
    pub environment_command: String,
    pub full_ref: String,
    pub full_command: &'static str,
}

#[derive(Debug, Serialize)]
pub struct RunnerListTruncation {
    pub shown: usize,
    pub omitted: usize,
    pub evidence_ref: &'static str,
    pub full_command: &'static str,
}

/// Runner-owned list payload. The generic CRUD output remains lossless for
/// entity commands while the default inventory can omit configuration maps.
#[derive(Debug, Serialize)]
pub struct RunnerListOutput {
    pub command: &'static str,
    pub variant: &'static str,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub runner_summaries: Vec<RunnerInventorySummary>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub entities: Vec<Runner>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub sessions: Vec<RunnerStatusReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub truncation: Option<RunnerListTruncation>,
}

/// Execution paths available to this controller, kept apart from concrete
/// runner identities and their connection lifecycle.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct RunnerExecutionCapabilities {
    pub local_placement: RunnerExecutionCapability,
    pub lab_runner_connection: LabRunnerConnectionCapability,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct RunnerExecutionCapability {
    pub available: bool,
    pub state: &'static str,
    pub next_action: String,
}

/// `available` reports Lab **admission** readiness (the same predicate cook
/// dispatch enforces via `resolve_parsed_command_preflight`), not raw session
/// connectivity. A runner can be connected without being admitted (stale
/// daemon, missing capabilities, capacity blocked, ...); in that case
/// `available` is `false` even though `connected_runner_ids` is non-empty, and
/// `reasons`/`next_action` explain what would admit it (#13631).
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct LabRunnerConnectionCapability {
    pub available: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub connected_runner_ids: Vec<String>,
    /// Non-empty exactly when at least one runner is connected but not
    /// admitted, explaining the gap between connectivity and dispatch
    /// readiness.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub reasons: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_action: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct RunnerTruncation {
    pub omitted_generations: usize,
    #[serde(skip_serializing_if = "is_zero")]
    pub omitted_sessions: usize,
    pub evidence_ref: String,
    pub full_command: String,
}

fn is_zero(value: &usize) -> bool {
    *value == 0
}

#[derive(Debug, Serialize)]
pub struct LabFollowup {
    pub label: String,
    pub command: String,
    pub purpose: String,
}

#[derive(Debug, Serialize)]
pub struct LabSelectedRunnerOutput {
    pub runner_id: String,
    pub kind: String,
    pub configured_executable: String,
    pub runner_homeboy: LabRunnerHomeboyOutput,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub executable_requirements: Vec<RunnerExecutableRequirementDiagnostics>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub runtime_diagnostics: Vec<RunnerRuntimeDiagnostics>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected_runtime: Option<SelectedRuntimeOutput>,
    pub daemon_enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_root: Option<String>,
    pub readiness_state: String,
    pub connected: bool,
    pub availability: RunnerAvailability,
    pub status: RunnerStatusReport,
}

#[derive(Debug, Serialize)]
pub struct LabRunnerHomeboyOutput {
    pub controller_version: String,
    pub controller_build_identity: String,
    pub configured_executable: String,
    pub controller_cli: RunnerHomeboyBinaryRole,
    pub active_daemon: RunnerHomeboyBinaryRole,
    pub configured_job_binary: RunnerHomeboyBinaryRole,
    pub binary_roles: Vec<RunnerHomeboyBinaryRole>,
    pub workflow_binary_guidance: RunnerWorkflowBinaryGuidance,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_daemon_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_daemon_build_identity: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job_command_binary_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job_command_binary_build_identity: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stale_daemon_severity: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stale_daemon_refresh_command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stale_daemon: Option<Value>,
    pub version_drift: bool,
    pub command_availability_checks: Vec<String>,
    pub artifact_features: RunnerArtifactFeatureDiagnostics,
    pub refresh_commands: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upgrade_command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dev_sync: Option<Value>,
}

/// One homeboy binary a runner resolves, as the operator status output renders it.
///
/// This was a second declaration of [`homeboy::runner::runners::RunnerBinarySource`]
/// -- the same six fields with the same serde attributes -- and
/// `runner_homeboy_binary_role` in `status.rs` copied them across one at a time
/// in a file that already imported both. `homeboy-cli` depends on
/// `homeboy-lab-runner` directly, so there was no boundary for the copy to
/// bridge; the CLI name is kept as an alias because the status output is where
/// operators read it.
pub type RunnerHomeboyBinaryRole = homeboy::runner::runners::RunnerBinarySource;

#[derive(Debug, Serialize)]
pub struct RunnerWorkflowBinaryGuidance {
    pub recent_workflows: &'static str,
    pub explicit_workflows: &'static str,
    pub capability_checks: &'static str,
}

#[derive(Debug, Serialize)]
pub struct RunnerArtifactFeatureDiagnostics {
    pub required_features: Vec<&'static str>,
    pub controller_commands: Vec<String>,
    pub runner_command_checks: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub hints: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RunnerToolDiagnostics {
    pub tool: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub configured_binary: Option<String>,
    pub configured_binary_source: String,
    pub managed_cache_source: String,
    pub managed_cache_binary: String,
    pub effective_binary_rule: String,
    pub diagnostic_command: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RunnerExecutableRequirementDiagnostics {
    pub runtime: String,
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub env: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub candidates: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub version_command: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub install_hint: Option<String>,
    pub diagnostic_state: &'static str,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SelectedRuntimeOutput {
    pub tool: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub configured_binary: Option<String>,
    pub configured_binary_source: String,
    pub managed_cache_source: String,
    pub managed_cache_binary: String,
    pub effective_binary_rule: String,
    pub primary_package: RuntimePackageOutput,
    pub secondary_package: RuntimePackageOutput,
    pub source_git_sha: RuntimeProbeValue,
    pub dist_build_freshness: RuntimeProbeValue,
    pub runtime_probe_command: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<RuntimeDiagnostic>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RunnerRuntimeDiagnostics {
    pub runtime: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub configured_binary: Option<String>,
    pub configured_binary_source: String,
    pub managed_cache_source: String,
    pub managed_cache_binary: String,
    pub effective_binary_rule: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub packages: Vec<RunnerRuntimePackageDiagnostics>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub probes: BTreeMap<String, RuntimeProbeValue>,
    pub runtime_probe_command: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<RuntimeDiagnostic>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RunnerRuntimePackageDiagnostics {
    pub field: String,
    pub package: String,
    pub expected_path: String,
    pub default_path: String,
    pub selection_source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env_override: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remediation_command: Option<String>,
    pub resolution: RuntimeProbeValue,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RuntimePackageOutput {
    pub package: String,
    pub expected_path: String,
    pub default_path: String,
    pub selection_source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env_override: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remediation_command: Option<String>,
    pub resolution: RuntimeProbeValue,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RuntimeProbeValue {
    pub value: Option<String>,
    pub source: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RuntimeDiagnostic {
    pub id: String,
    pub severity: String,
    pub message: String,
    pub remediation: String,
}

#[derive(Debug, Serialize)]
pub struct RunnerOperatorCommand {
    pub scope: &'static str,
    pub runner_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job_id: Option<String>,
    pub command: String,
    pub description: String,
}

#[derive(Debug, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum RunnerConnectionOutput {
    Connect(Box<RunnerConnectReport>),
    Status(Box<RunnerStatusReport>),
    Disconnect(Box<RunnerDisconnectReport>),
}

/// The postcondition reached by `runner disconnect`.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunnerDisconnectStatus {
    Disconnected,
    LocalRecovery,
    AlreadyDisconnected,
    PartialFailure,
}

pub type RunnerOutput = EntityCrudOutput<Runner, RunnerExtra>;

pub(super) const REDACTED_ENV_VALUE: &str = "[redacted]";

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum RunnerCommandOutput {
    List(Box<RunnerListOutput>),
    Registry(Box<RunnerOutput>),
    Doctor(Box<serde_json::Value>),
    Preflight(Box<homeboy::runner::runners::PlacementReadiness>),
    Execution(Box<RunnerExecutionCommandOutput>),
    Env(Box<RunnerEnvOutput>),
    RecipeRunProviders(Box<RecipeRunProvidersOutput>),
    Lifecycle(Box<lifecycle::RunnerLifecycleOutput>),
    JobList(Box<RunnerJobListOutput>),
    Job(Box<RunnerJobOutput>),
    BrokerJob(Box<RunnerBrokerJobOutput>),
    RefreshHomeboy(Box<RunnerRefreshHomeboyCommandOutput>),
    DevSync(Box<homeboy::runner::runners::RunnerDevSyncOutput>),
    CachePrune(Box<homeboy::runner::runners::RunnerBinaryCachePruneOutput>),
    Worker(Box<ReverseRunnerWorkerOutput>),
    Workspace(Box<workspace::RunnerWorkspaceOutput>),
    RefreshPlan(Box<refresh_plan::LabRefreshPlanOutput>),
    Broker(Box<RunnerBrokerOutput>),
}

#[derive(Debug, Serialize)]
pub struct RecipeRunProvidersOutput {
    pub variant: &'static str,
    pub command: &'static str,
    pub providers: Vec<homeboy_core::extension::RecipeRunProviderInventoryEntry>,
}

/// An authoritative job observation or a retained generation ownership record.
/// Retained projections deliberately omit fields the daemon no longer provides.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct RunnerJobListEntry {
    pub job_id: String,
    pub source: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub daemon_source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<JobStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub correlation: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generation: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logs_command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cancel_command: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct RunnerJobListOutput {
    pub variant: &'static str,
    pub command: &'static str,
    pub runner_id: String,
    /// Live entries are a read-only daemon/broker observation at invocation time.
    pub live_daemon_job_count: usize,
    /// Retained entries are durable generation ownership records, not evidence
    /// that a process is still running.
    pub retained_durable_projection_count: usize,
    pub jobs: Vec<RunnerJobListEntry>,
}

#[derive(Debug, Serialize)]
pub struct RunnerExecutionCommandOutput {
    #[serde(flatten)]
    pub output: RunnerExecOutput,
    #[serde(
        rename = "_homeboy_actionable",
        skip_serializing_if = "Option::is_none"
    )]
    pub actionable: Option<CommandActionableMetadata>,
}

#[derive(Debug, Serialize)]
pub struct RunnerRefreshHomeboyCommandOutput {
    #[serde(flatten)]
    pub output: homeboy::runner::runners::HomeboyBinaryRefreshOutput,
    #[serde(
        rename = "_homeboy_actionable",
        skip_serializing_if = "Option::is_none"
    )]
    pub actionable: Option<CommandActionableMetadata>,
}

/// Result of a broker auth/pairing management command. The plaintext `token` is
/// present only on a successful `pair` and is the single time it is ever shown.
#[derive(Debug, Serialize)]
pub struct RunnerBrokerOutput {
    pub command: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credential_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runner_id: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub scopes: Vec<String>,
    /// One-time plaintext bearer token (only on `pair`). Never re-displayed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revoked: Option<bool>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub credentials: Vec<RunnerBrokerCredentialSummary>,
    pub store_path: String,
}

/// Non-secret summary of a stored broker credential. Token hashes are never
/// surfaced.
#[derive(Debug, Serialize)]
pub struct RunnerBrokerCredentialSummary {
    pub id: String,
    pub runner_id: String,
    pub scopes: Vec<String>,
    pub revoked: bool,
    pub created_at: String,
}

#[derive(Debug, Serialize)]
pub struct RunnerJobOutput {
    pub variant: &'static str,
    pub command: &'static str,
    pub runner_id: String,
    pub job_id: String,
    pub follow: bool,
    /// True when the payload was projected to lifecycle events + exit code +
    /// bounded stdout/stderr tails rather than the full job record.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub compact: bool,
    pub job: Job,
    pub runner_job: homeboy::runner::runners::RunnerJob,
    pub events: Vec<JobEvent>,
    /// Exit code lifted out of the structured result event. Surfaced only in
    /// compact/tail projections so callers can read "exit N" without the blob.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub orchestration_provenance: Option<Value>,
    /// Bounded stdout view. Present only when the raw stdout was stripped from
    /// `events` (compact or `--tail`); otherwise stdout lives once in the
    /// structured result event.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stdout: Option<RunnerJobLogStream>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stderr: Option<RunnerJobLogStream>,
    /// Highest event sequence emitted by this invocation.
    pub next_cursor: u64,
    /// Copyable continuation command, including the monotonic event cursor.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resume_command: Option<String>,
}

/// A bounded view of a captured output stream. `tail` holds at most
/// `returned_bytes` of the trailing output; `total_bytes` is the full size so
/// callers know how much was elided.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RunnerJobLogStream {
    pub total_bytes: usize,
    pub returned_bytes: usize,
    pub truncated: bool,
    pub tail: String,
}

#[derive(Debug, Serialize)]
pub struct RunnerBrokerJobOutput {
    pub variant: &'static str,
    pub command: &'static str,
    pub runner_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact_id: Option<String>,
    pub response: Value,
}

#[derive(Debug, Serialize)]
pub struct RunnerEnvOutput {
    pub variant: &'static str,
    pub command: String,
    pub runner_id: String,
    pub source: String,
    pub values_redacted: bool,
    pub env: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub secret_env: BTreeMap<String, RunnerSecretEnvReferenceOutput>,
    pub diagnostics: RunnerEnvDiagnostics,
}

#[derive(Debug, Serialize)]
pub struct RunnerSecretEnvReferenceOutput {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secret: Option<String>,
    pub values_redacted: bool,
}

#[derive(Debug, Serialize)]
pub struct RunnerEnvDiagnostics {
    pub server_shell_env: String,
    pub runner_job_env: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected_tool: Option<RunnerToolDiagnostics>,
}
