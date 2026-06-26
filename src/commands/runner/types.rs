use serde::Serialize;
use serde_json::Value;

use homeboy::core::api_jobs::{Job, JobEvent};
use homeboy::core::runners::{
    ReverseRunnerWorkerOutput, Runner, RunnerConnectReport, RunnerDisconnectReport,
    RunnerExecOutput, RunnerStatusReport,
};
use homeboy::core::EntityCrudOutput;

use std::collections::BTreeMap;

use super::doctor;
use super::workspace;

#[derive(Debug, Serialize)]
pub struct RunnerExtra {
    pub variant: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preferred_lab_runner: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected_lab_runner: Option<LabSelectedRunnerOutput>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub managed_followups: Vec<LabFollowup>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connection: Option<RunnerConnectionOutput>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub sessions: Vec<RunnerStatusReport>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub operator_hints: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub operator_commands: Vec<RunnerOperatorCommand>,
}

impl Default for RunnerExtra {
    fn default() -> Self {
        Self {
            variant: "registry",
            preferred_lab_runner: None,
            selected_lab_runner: None,
            managed_followups: Vec::new(),
            connection: None,
            sessions: Vec::new(),
            operator_hints: Vec::new(),
            operator_commands: Vec::new(),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct LabFollowup {
    pub label: &'static str,
    pub command: String,
    pub purpose: &'static str,
}

#[derive(Debug, Serialize)]
pub struct LabSelectedRunnerOutput {
    pub runner_id: String,
    pub kind: String,
    pub configured_executable: String,
    pub runner_homeboy: LabRunnerHomeboyOutput,
    pub wp_codebox_runtime: WpCodeboxRuntimeOutput,
    pub daemon_enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_root: Option<String>,
    pub readiness_state: String,
    pub connected: bool,
    pub status: RunnerStatusReport,
}

#[derive(Debug, Serialize)]
pub struct LabRunnerHomeboyOutput {
    pub controller_version: String,
    pub controller_build_identity: String,
    pub configured_executable: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_daemon_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_daemon_build_identity: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stale_daemon: Option<Value>,
    pub version_drift: bool,
    pub command_availability_checks: Vec<String>,
    pub artifact_features: RunnerArtifactFeatureDiagnostics,
    pub refresh_commands: Vec<String>,
    pub upgrade_command: String,
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
    pub tool: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub configured_binary: Option<String>,
    pub configured_binary_source: &'static str,
    pub managed_cache_source: String,
    pub managed_cache_binary: String,
    pub effective_binary_rule: &'static str,
    pub diagnostic_command: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct WpCodeboxRuntimeOutput {
    pub tool: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub configured_binary: Option<String>,
    pub configured_binary_source: &'static str,
    pub managed_cache_source: String,
    pub managed_cache_binary: String,
    pub effective_binary_rule: &'static str,
    pub playground_package: WpCodeboxPackageRuntimeOutput,
    pub core_package: WpCodeboxPackageRuntimeOutput,
    pub source_git_sha: WpCodeboxProbeValue,
    pub dist_build_freshness: WpCodeboxProbeValue,
    pub runtime_probe_command: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<WpCodeboxRuntimeDiagnostic>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct WpCodeboxPackageRuntimeOutput {
    pub package: &'static str,
    pub expected_path: String,
    pub resolution: WpCodeboxProbeValue,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct WpCodeboxProbeValue {
    pub value: Option<String>,
    pub source: &'static str,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct WpCodeboxRuntimeDiagnostic {
    pub id: &'static str,
    pub severity: &'static str,
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
    Connect(RunnerConnectReport),
    Status(RunnerStatusReport),
    Disconnect(RunnerDisconnectReport),
}

pub type RunnerOutput = EntityCrudOutput<Runner, RunnerExtra>;

pub(super) const REDACTED_ENV_VALUE: &str = "[redacted]";
pub(super) const RUNNER_EXEC_SCRIPT_ENV: &str = "HOMEBOY_RUNNER_EXEC_SCRIPT";

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum RunnerCommandOutput {
    Registry(RunnerOutput),
    Doctor(doctor::RunnerDoctorOutput),
    Execution(RunnerExecOutput),
    Env(RunnerEnvOutput),
    Job(RunnerJobOutput),
    BrokerJob(RunnerBrokerJobOutput),
    RefreshHomeboy(homeboy::core::runners::HomeboyBinaryRefreshOutput),
    Worker(ReverseRunnerWorkerOutput),
    Workspace(workspace::RunnerWorkspaceOutput),
    Broker(RunnerBrokerOutput),
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
    pub job: Job,
    pub runner_job: homeboy::core::runners::RunnerJob,
    pub events: Vec<JobEvent>,
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
    pub wp_codebox: Option<RunnerToolDiagnostics>,
}
