use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
#[cfg(not(test))]
use std::sync::{OnceLock, RwLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::core::agent_runtime_manifest::AgentRuntimeDiscoveryDiagnostic;
use crate::core::agent_task::{
    AgentTaskArtifact, AgentTaskDiagnostic, AgentTaskEvidenceRef, AgentTaskExecutionState,
    AgentTaskFailureClassification, AgentTaskOutcome, AgentTaskOutcomeStatus, AgentTaskRequest,
    AgentTaskTypedArtifact, AGENT_TASK_ARTIFACT_SCHEMA, AGENT_TASK_OUTCOME_SCHEMA,
    AGENT_TASK_REQUEST_SCHEMA, AGENT_TOOL_POLICY_SCHEMA, AGENT_TOOL_REQUEST_SCHEMA,
    AGENT_TOOL_RESULT_SCHEMA,
};
use crate::core::agent_task_gate_executor::{is_repo_local_gate_request, run_repo_local_gate_task};
use crate::core::agent_task_scheduler::{
    AgentTaskExecutionContext, AgentTaskExecutorAdapter, AgentTaskPlan,
};
use crate::core::agent_task_secrets::{
    resolve_secret_env_with_fallbacks, secret_env_status_with_fallbacks,
    AgentTaskSecretResolutionError,
};
use crate::core::agent_task_timeout::timeout_with_grace;
use crate::core::command_invocation::CommandInvocation;
use crate::core::engine::shell;
use crate::core::secret_env_plan::{SecretEnvPlan, SecretEnvStatus};
use crate::core::{agent_runtime_manifest, component, defaults, extension, Error};

pub const AGENT_TASK_EXECUTOR_PROVIDER_SCHEMA: &str = "homeboy/agent-task-executor-provider/v1";
pub const AGENT_TASK_PROVIDER_CAPABILITY_CONTRACT_SCHEMA: &str =
    "homeboy/agent-task-provider-capability-contract/v1";

#[cfg(not(test))]
static PROVIDER_CATALOG: OnceLock<RwLock<AgentTaskProviderCatalog>> = OnceLock::new();

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskProviderCapabilityContract {
    pub schema: String,
    pub provider_schema: String,
    pub request_schema: String,
    pub outcome_schema: String,
    pub tool_request_schema: String,
    pub tool_result_schema: String,
    pub tool_policy_schema: String,
}

pub fn provider_capability_contract() -> AgentTaskProviderCapabilityContract {
    AgentTaskProviderCapabilityContract {
        schema: AGENT_TASK_PROVIDER_CAPABILITY_CONTRACT_SCHEMA.to_string(),
        provider_schema: AGENT_TASK_EXECUTOR_PROVIDER_SCHEMA.to_string(),
        request_schema: AGENT_TASK_REQUEST_SCHEMA.to_string(),
        outcome_schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
        tool_request_schema: AGENT_TOOL_REQUEST_SCHEMA.to_string(),
        tool_result_schema: AGENT_TOOL_RESULT_SCHEMA.to_string(),
        tool_policy_schema: AGENT_TOOL_POLICY_SCHEMA.to_string(),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskExecutorProvider {
    #[serde(default = "default_provider_schema")]
    pub schema: String,
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub backend: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub default_backend: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub command: String,
    #[serde(default, alias = "argv", skip_serializing_if = "Vec::is_empty")]
    pub command_argv: Vec<String>,
    #[serde(default, skip_serializing_if = "CommandInvocation::is_empty")]
    pub invocation: CommandInvocation,
    #[serde(default = "default_request_schema")]
    pub request_schema: String,
    #[serde(default = "default_outcome_schema")]
    pub outcome_schema: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub secret_requirements: Vec<AgentTaskProviderSecretRequirement>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub secret_env_requirements: Vec<AgentTaskProviderSecretEnvRequirement>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_materialization: Option<AgentTaskProviderWorkspaceMaterialization>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub provider_defaults: BTreeMap<String, Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runner_readiness: Vec<AgentTaskProviderRunnerReadiness>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runner_sources: Vec<AgentTaskProviderRunnerSource>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependency_failure_patterns: Vec<AgentTaskProviderDependencyFailurePattern>,
    #[serde(
        default,
        skip_serializing_if = "AgentTaskProviderTimeoutArtifactDiscovery::is_empty"
    )]
    pub timeout_artifact_discovery: AgentTaskProviderTimeoutArtifactDiscovery,
    #[serde(
        default,
        skip_serializing_if = "AgentTaskProviderRoleAliases::is_empty"
    )]
    pub role_aliases: AgentTaskProviderRoleAliases,
    #[serde(default, skip_serializing_if = "AgentTaskRuntimeContract::is_empty")]
    pub runtime_contract: AgentTaskRuntimeContract,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extension_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extension_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_path: Option<String>,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AgentTaskRuntimeContract {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
    #[serde(
        default,
        skip_serializing_if = "AgentTaskRuntimeLifecycleStates::is_empty"
    )]
    pub lifecycle_states: AgentTaskRuntimeLifecycleStates,
    #[serde(
        default,
        skip_serializing_if = "AgentTaskRuntimeNormalization::is_empty"
    )]
    pub normalization: AgentTaskRuntimeNormalization,
    #[serde(default, skip_serializing_if = "AgentTaskRuntimeApplyBack::is_empty")]
    pub apply_back: AgentTaskRuntimeApplyBack,
}

impl AgentTaskRuntimeContract {
    fn is_empty(&self) -> bool {
        self.capabilities.is_empty()
            && self.lifecycle_states.is_empty()
            && self.normalization.is_empty()
            && self.apply_back.is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AgentTaskRuntimeApplyBack {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mutation_artifacts: Vec<AgentTaskRuntimeMutationArtifact>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requires_git_checkout: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strategy: Option<String>,
}

impl AgentTaskRuntimeApplyBack {
    fn is_empty(&self) -> bool {
        self.mutation_artifacts.is_empty()
            && self.requires_git_checkout.is_none()
            && self.strategy.is_none()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AgentTaskRuntimeMutationArtifact {
    pub name: String,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub apply_method: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AgentTaskRuntimeLifecycleStates {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub execution_states: BTreeMap<String, AgentTaskExecutionState>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub outcome_statuses: BTreeMap<String, AgentTaskOutcomeStatus>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub failure_classifications: BTreeMap<String, AgentTaskFailureClassification>,
}

impl AgentTaskRuntimeLifecycleStates {
    fn is_empty(&self) -> bool {
        self.execution_states.is_empty()
            && self.outcome_statuses.is_empty()
            && self.failure_classifications.is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AgentTaskRuntimeNormalization {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary_path: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub output_artifacts: Vec<AgentTaskRuntimeOutputArtifactMapping>,
}

impl AgentTaskRuntimeNormalization {
    fn is_empty(&self) -> bool {
        self.status_path.is_none()
            && self.summary_path.is_none()
            && self.output_artifacts.is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AgentTaskRuntimeOutputArtifactMapping {
    pub name: String,
    #[serde(
        default,
        rename = "type",
        alias = "artifact_type",
        skip_serializing_if = "Option::is_none"
    )]
    pub artifact_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_schema: Option<String>,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
}

fn default_provider_schema() -> String {
    AGENT_TASK_EXECUTOR_PROVIDER_SCHEMA.to_string()
}

fn default_request_schema() -> String {
    AGENT_TASK_REQUEST_SCHEMA.to_string()
}

fn default_outcome_schema() -> String {
    AGENT_TASK_OUTCOME_SCHEMA.to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AgentTaskProviderSecretRequirement {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, deserialize_with = "deserialize_string_or_vec")]
    pub env: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purpose: Option<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AgentTaskProviderSecretEnvRequirement {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, deserialize_with = "deserialize_string_or_vec")]
    pub env: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<Value>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

fn deserialize_string_or_vec<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<Value>::deserialize(deserializer)?;
    Ok(match value {
        Some(Value::String(item)) => vec![item],
        Some(Value::Array(items)) => items
            .into_iter()
            .filter_map(|item| item.as_str().map(str::to_string))
            .collect(),
        _ => Vec::new(),
    })
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskProviderRunnerReadiness {
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub secret_env: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env_path: Option<AgentTaskProviderEnvPathReadiness>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executable: Option<AgentTaskProviderExecutableReadiness>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remediation: Option<String>,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskProviderEnvPathReadiness {
    pub env: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<bool>,
    /// Optional extension-declared canonical root (e.g. a managed source clone
    /// kept under the runner's homeboy cache). When set, doctor WARNS if the
    /// env-resolved path does not live under this canonical root, catching
    /// stale / non-canonical checkout drift before it corrupts results.
    ///
    /// Core is runtime-agnostic: it does not know what the canonical path
    /// represents (a runtime checkout, a toolchain, etc.) — the value is supplied
    /// entirely by the declaring extension.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canonical_path: Option<String>,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskProviderExecutableReadiness {
    /// Environment variable names that should receive the resolved executable path.
    /// Existing non-empty env values win before binary candidate discovery.
    pub env: Vec<String>,
    /// Ordered executable names or paths to try when env does not already point
    /// at an executable. Bare names are resolved on PATH; paths are checked as-is.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub candidates: Vec<String>,
    /// Optional arguments a caller can use to probe the resolved executable version.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub version_command: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub install_hint: Option<String>,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AgentTaskProviderResolvedExecutable {
    env: Vec<String>,
    path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AgentTaskProviderExecutableResolutionError {
    readiness_id: String,
    label: String,
    env: Vec<String>,
    candidates: Vec<String>,
    install_hint: Option<String>,
}

impl AgentTaskProviderExecutableResolutionError {
    fn message(&self) -> String {
        let mut message = format!(
            "provider runner executable '{}' is not configured",
            self.label
        );
        if !self.env.is_empty() {
            message.push_str(&format!("; set one of: {}", self.env.join(", ")));
        }
        if !self.candidates.is_empty() {
            message.push_str(&format!(
                "; searched candidates: {}",
                self.candidates.join(", ")
            ));
        }
        if let Some(hint) = self.install_hint.as_deref().filter(|hint| !hint.is_empty()) {
            message.push_str(&format!("; install hint: {hint}"));
        }
        message
    }
}

/// A named, extension-declared source checkout that homeboy keeps synced on the
/// runner. Core treats this generically: it materializes/refreshes a git
/// checkout to the intended ref/remote. It has no knowledge of what the source
/// is (a runtime checkout, a CLI, a toolchain) — extensions declare the path/remote/ref.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskProviderRunnerSource {
    pub id: String,
    pub label: String,
    /// Absolute path (or `$HOME`/`~`-prefixed path) of the managed checkout on
    /// the runner, e.g. a path under the runner's homeboy cache directory.
    pub path: String,
    /// Optional canonical remote URL the checkout must track. When set, homeboy
    /// re-points `origin` if the checkout tracks a different remote (fixing the
    /// "tracks wrong remote" drift), then fetches and fast-forwards.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_url: Option<String>,
    /// Optional explicit ref (branch, tag, or sha) to check out and sync to.
    /// When omitted, homeboy fast-forwards the current branch to its upstream.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remediation: Option<String>,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskProviderDependencyFailurePattern {
    pub id: String,
    pub label: String,
    pub path_contains: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub error_contains_any: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remediation: Option<String>,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AgentTaskProviderTimeoutArtifactDiscovery {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub metadata_path_keys: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub config_path_keys: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_patterns: Vec<AgentTaskProviderArtifactPattern>,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

impl AgentTaskProviderTimeoutArtifactDiscovery {
    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
            && self.metadata_path_keys.is_empty()
            && self.config_path_keys.is_empty()
            && self.artifact_patterns.is_empty()
            && self.extra.is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AgentTaskProviderArtifactPattern {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub filename_patterns: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub filename_contains: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extensions: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime: Option<String>,
    #[serde(
        default = "default_metadata",
        skip_serializing_if = "is_empty_metadata"
    )]
    pub metadata: Value,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

fn default_metadata() -> Value {
    Value::Object(Default::default())
}

fn is_empty_metadata(value: &Value) -> bool {
    value.as_object().is_none_or(|object| object.is_empty())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AgentTaskProviderRoleAliases {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub artifact_kinds: BTreeMap<String, Vec<String>>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub artifact_filenames: BTreeMap<String, Vec<String>>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub outputs: BTreeMap<String, Vec<String>>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, Vec<String>>,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

impl AgentTaskProviderRoleAliases {
    pub fn is_empty(&self) -> bool {
        self.artifact_kinds.is_empty()
            && self.artifact_filenames.is_empty()
            && self.outputs.is_empty()
            && self.metadata.is_empty()
            && self.extra.is_empty()
    }

    pub fn artifact_kind_matches_role(&self, role: &str, kind: &str) -> bool {
        alias_matches(self.artifact_kinds.get(role), kind)
    }

    pub fn artifact_filename_matches_role(&self, role: &str, filename: &str) -> bool {
        alias_matches(self.artifact_filenames.get(role), filename)
    }

    fn role_for_artifact_kind(&self, kind: &str) -> Option<&str> {
        self.artifact_kinds
            .iter()
            .find_map(|(role, aliases)| alias_matches(Some(aliases), kind).then_some(role.as_str()))
    }

    pub fn output_aliases_for_role(&self, role: &str) -> Vec<&str> {
        aliases_for_role(&self.outputs, role)
    }

    pub fn metadata_aliases_for_role(&self, role: &str) -> Vec<&str> {
        aliases_for_role(&self.metadata, role)
    }
}

fn aliases_for_role<'a>(map: &'a BTreeMap<String, Vec<String>>, role: &str) -> Vec<&'a str> {
    map.get(role)
        .into_iter()
        .flatten()
        .map(String::as_str)
        .collect()
}

fn alias_matches(aliases: Option<&Vec<String>>, value: &str) -> bool {
    let value = value.to_ascii_lowercase();
    aliases.into_iter().flatten().any(|alias| {
        let alias = alias.to_ascii_lowercase();
        alias == value || wildcard_match(&alias, &value)
    })
}

/// Shared glob-style matcher used by provider role-alias resolution and the
/// timeout artifact discovery scanner. Supports `*` wildcards with optional
/// start/end anchoring.
pub(crate) fn wildcard_match(pattern: &str, value: &str) -> bool {
    if !pattern.contains('*') {
        return pattern == value;
    }

    let mut remainder = value;
    let anchored_start = !pattern.starts_with('*');
    let anchored_end = !pattern.ends_with('*');
    let parts: Vec<&str> = pattern.split('*').filter(|part| !part.is_empty()).collect();
    if parts.is_empty() {
        return true;
    }
    if anchored_start && !value.starts_with(parts[0]) {
        return false;
    }
    for part in &parts {
        let Some(index) = remainder.find(part) else {
            return false;
        };
        remainder = &remainder[index + part.len()..];
    }
    !anchored_end || value.ends_with(parts[parts.len() - 1])
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AgentTaskProviderWorkspaceMaterialization {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requires_git: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub write_scope: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_paths: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spec: Option<WorkspaceMaterializationSpec>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mounts: Vec<WorkspaceMountSpec>,
    #[serde(default, skip_serializing_if = "AgentTaskRuntimeApplyBack::is_empty")]
    pub apply_back: AgentTaskRuntimeApplyBack,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

impl AgentTaskProviderWorkspaceMaterialization {
    pub fn requires_cwd_git_checkout(&self) -> bool {
        self.apply_back.requires_git_checkout == Some(true)
            || self.requires_git == Some(true)
            || self.cwd.as_deref() == Some("git_checkout")
    }

    pub fn validate(&self) -> Result<(), Vec<String>> {
        let mut errors = Vec::new();
        if let Some(spec) = &self.spec {
            errors.extend(
                spec.validation_errors()
                    .into_iter()
                    .map(|error| format!("spec.{error}")),
            );
        }
        for (index, mount) in self.mounts.iter().enumerate() {
            errors.extend(
                mount
                    .validation_errors()
                    .into_iter()
                    .map(|error| format!("mounts[{index}].{error}")),
            );
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct WorkspaceMaterializationSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handle: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub materialization: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mounts: Vec<WorkspaceMountSpec>,
    #[serde(
        default = "default_metadata",
        skip_serializing_if = "is_empty_metadata"
    )]
    pub metadata: Value,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

impl WorkspaceMaterializationSpec {
    pub fn validate(&self) -> Result<(), Vec<String>> {
        let errors = self.validation_errors();
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    fn validation_errors(&self) -> Vec<String> {
        let mut errors = workspace_mount_like_validation_errors(
            self.handle.as_deref(),
            self.repo.as_deref(),
            self.host_path.as_deref(),
            self.target_path.as_deref(),
            self.mode.as_deref(),
            self.materialization.as_deref(),
        );
        for (index, mount) in self.mounts.iter().enumerate() {
            errors.extend(
                mount
                    .validation_errors()
                    .into_iter()
                    .map(|error| format!("mounts[{index}].{error}")),
            );
        }
        errors
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct WorkspaceMountSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handle: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub materialization: Option<String>,
    #[serde(
        default = "default_metadata",
        skip_serializing_if = "is_empty_metadata"
    )]
    pub metadata: Value,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

impl WorkspaceMountSpec {
    pub fn validate(&self) -> Result<(), Vec<String>> {
        let errors = self.validation_errors();
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    fn validation_errors(&self) -> Vec<String> {
        workspace_mount_like_validation_errors(
            self.handle.as_deref(),
            self.repo.as_deref(),
            self.host_path.as_deref(),
            self.target_path.as_deref(),
            self.mode.as_deref(),
            self.materialization.as_deref(),
        )
    }
}

fn workspace_mount_like_validation_errors(
    handle: Option<&str>,
    repo: Option<&str>,
    host_path: Option<&str>,
    target_path: Option<&str>,
    mode: Option<&str>,
    materialization: Option<&str>,
) -> Vec<String> {
    let mut errors = Vec::new();
    validate_non_blank_optional("handle", handle, &mut errors);
    validate_non_blank_optional("repo", repo, &mut errors);
    validate_non_blank_optional("host_path", host_path, &mut errors);
    validate_non_blank_optional("target_path", target_path, &mut errors);
    validate_non_blank_optional("mode", mode, &mut errors);
    validate_non_blank_optional("materialization", materialization, &mut errors);
    if host_path.is_some() && target_path.is_none() {
        errors.push("target_path is required when host_path is set".to_string());
    }
    errors
}

fn validate_non_blank_optional(field: &str, value: Option<&str>, errors: &mut Vec<String>) {
    if value.is_some_and(|value| value.trim().is_empty()) {
        errors.push(format!("{field} must not be blank"));
    }
}

#[derive(Debug, Clone, Default)]
pub struct ExtensionProviderAgentTaskExecutor {
    providers: Vec<AgentTaskExecutorProvider>,
    diagnostics: Vec<AgentRuntimeDiscoveryDiagnostic>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskProviderCatalog {
    pub providers: Vec<AgentTaskExecutorProvider>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<AgentRuntimeDiscoveryDiagnostic>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

impl AgentTaskProviderCatalog {
    pub fn discover() -> Self {
        #[cfg(not(test))]
        {
            let catalog = PROVIDER_CATALOG.get_or_init(|| RwLock::new(discover_provider_catalog()));
            return catalog.read().expect("provider catalog lock").clone();
        }
        #[cfg(test)]
        {
            discover_provider_catalog()
        }
    }

    pub fn refresh() -> Self {
        #[cfg(not(test))]
        {
            let refreshed = discover_provider_catalog();
            let catalog = PROVIDER_CATALOG.get_or_init(|| RwLock::new(refreshed.clone()));
            *catalog.write().expect("provider catalog lock") = refreshed.clone();
            return refreshed;
        }
        #[cfg(test)]
        {
            discover_provider_catalog()
        }
    }

    pub fn providers(&self) -> &[AgentTaskExecutorProvider] {
        &self.providers
    }

    pub fn diagnostics(&self) -> &[AgentRuntimeDiscoveryDiagnostic] {
        &self.diagnostics
    }

    pub fn provider_requires_cwd_git_checkout(
        &self,
        backend: &str,
        selector: Option<&str>,
    ) -> bool {
        provider_requires_cwd_git_checkout_with_providers(&self.providers, backend, selector)
    }

    pub fn apply_provider_runner_secret_env_contracts(&self, plan: &mut AgentTaskPlan) {
        apply_provider_runner_secret_env_contracts_with_providers(plan, &self.providers);
    }

    pub fn provider_secret_sources_for_providers(
        &self,
    ) -> HashMap<String, defaults::AgentTaskSecretSource> {
        provider_secret_sources_for_providers(&self.providers)
    }
}

fn discover_provider_catalog() -> AgentTaskProviderCatalog {
    let catalog = agent_runtime_manifest::discover_agent_task_executor_provider_catalog();
    AgentTaskProviderCatalog {
        providers: catalog.providers,
        diagnostics: catalog.diagnostics,
        version: Some(format!(
            "discovered:{}",
            chrono::Utc::now().timestamp_millis()
        )),
    }
}

impl ExtensionProviderAgentTaskExecutor {
    pub fn discover() -> Self {
        Self::from_catalog(AgentTaskProviderCatalog::discover())
    }

    pub fn from_catalog(catalog: AgentTaskProviderCatalog) -> Self {
        Self {
            providers: catalog.providers,
            diagnostics: catalog.diagnostics,
        }
    }

    #[cfg(test)]
    fn with_providers(providers: Vec<AgentTaskExecutorProvider>) -> Self {
        Self {
            providers,
            diagnostics: Vec::new(),
        }
    }

    pub fn providers(&self) -> &[AgentTaskExecutorProvider] {
        &self.providers
    }

    pub fn diagnostics(&self) -> &[AgentRuntimeDiscoveryDiagnostic] {
        &self.diagnostics
    }

    pub fn default_backend(&self) -> crate::core::Result<Option<String>> {
        default_backend_from_policy(None)
    }

    pub fn required_extension_ids_for_plan(&self, plan: &AgentTaskPlan) -> Vec<String> {
        required_extension_ids_for_plan_with_providers(plan, &self.providers)
    }
}

pub fn default_backend() -> crate::core::Result<Option<String>> {
    default_backend_from_policy(None)
}

pub fn default_backend_for_component(
    component_id: Option<&str>,
) -> crate::core::Result<Option<String>> {
    default_backend_from_policy(component_id)
}

pub fn provider_runner_readiness_contracts() -> Vec<AgentTaskProviderRunnerReadiness> {
    AgentTaskProviderCatalog::discover()
        .providers
        .into_iter()
        .flat_map(|provider| provider.runner_readiness)
        .collect()
}

pub fn provider_runner_source_contracts() -> Vec<AgentTaskProviderRunnerSource> {
    AgentTaskProviderCatalog::discover()
        .providers
        .into_iter()
        .flat_map(|provider| provider.runner_sources)
        .collect()
}

pub fn dependency_failure_patterns() -> Vec<AgentTaskProviderDependencyFailurePattern> {
    AgentTaskProviderCatalog::discover()
        .providers
        .into_iter()
        .flat_map(|provider| provider.dependency_failure_patterns)
        .collect()
}

pub fn validate_provider_runner_readiness_for_backend(
    backend: &str,
    selector: Option<&str>,
) -> crate::core::Result<()> {
    let providers = discover_agent_task_executor_providers();
    validate_provider_runner_readiness_for_backend_with_providers(&providers, backend, selector)
}

fn validate_provider_runner_readiness_for_backend_with_providers(
    providers: &[AgentTaskExecutorProvider],
    backend: &str,
    selector: Option<&str>,
) -> crate::core::Result<()> {
    let provider = match resolve_provider_for_backend(providers, backend, selector) {
        ProviderResolution::Resolved(provider) => provider,
        ProviderResolution::NotFound => {
            return Err(Error::validation_invalid_argument(
                "backend",
                format!("no extension agent-task provider found for backend '{backend}'"),
                Some(backend.to_string()),
                Some(vec![
                    "Run `homeboy agent-task providers` on the same machine/runner to inspect registered providers.".to_string(),
                    "Install or sync the extension/runtime that declares the requested backend, or pass --backend with a registered backend.".to_string(),
                ]),
            ));
        }
        ProviderResolution::AmbiguousExtensionAlias { candidate_ids } => {
            return Err(Error::validation_invalid_argument(
                "backend",
                format!(
                    "backend '{backend}' matches multiple extension agent-task providers; pass --selector with one provider id"
                ),
                Some(backend.to_string()),
                Some(vec![format!(
                    "Available provider ids for selector: {}.",
                    candidate_ids.join(", ")
                )]),
            ));
        }
        ProviderResolution::SelectorMismatch { available_ids } => {
            return Err(Error::validation_invalid_argument(
                "selector",
                format!(
                    "no extension agent-task provider for backend '{backend}' matched selector '{}'",
                    selector.unwrap_or("")
                ),
                selector.map(str::to_string),
                Some(vec![format!(
                    "Available provider ids for backend '{backend}': {}.",
                    available_ids.join(", ")
                )]),
            ));
        }
    };

    provider_executable_env(provider).map_err(|error| {
        Error::validation_invalid_argument(
            "backend",
            format!(
                "agent-task backend '{backend}' is registered but runner readiness failed for provider '{}': {}",
                provider.id,
                error.message()
            ),
            Some(backend.to_string()),
            Some(vec![
                format!(
                    "Selected provider: {} (backend '{}', selector '{}').",
                    provider.id,
                    provider.backend,
                    selector.unwrap_or("<default>")
                ),
                "Fix the executable/env on this machine or runner before dispatching the task wave.".to_string(),
            ]),
        )
    })?;

    Ok(())
}

pub fn required_extension_ids_for_plan(plan: &AgentTaskPlan) -> Vec<String> {
    ExtensionProviderAgentTaskExecutor::discover().required_extension_ids_for_plan(plan)
}

pub fn provider_requires_cwd_git_checkout(backend: &str, selector: Option<&str>) -> bool {
    AgentTaskProviderCatalog::discover().provider_requires_cwd_git_checkout(backend, selector)
}

pub fn apply_provider_runner_secret_env_contracts(plan: &mut AgentTaskPlan) {
    AgentTaskProviderCatalog::discover().apply_provider_runner_secret_env_contracts(plan);
}

pub fn provider_runner_secret_env_for_plan(plan: &AgentTaskPlan) -> Vec<String> {
    let catalog = AgentTaskProviderCatalog::discover();
    provider_runner_secret_env_for_plan_with_providers(plan, catalog.providers())
}

pub fn provider_secret_sources_for_plan(
    plan: &AgentTaskPlan,
) -> HashMap<String, defaults::AgentTaskSecretSource> {
    let catalog = AgentTaskProviderCatalog::discover();
    provider_secret_sources_for_plan_with_providers(plan, catalog.providers())
}

pub fn provider_secret_sources_for_discovered_providers(
) -> HashMap<String, defaults::AgentTaskSecretSource> {
    AgentTaskProviderCatalog::discover().provider_secret_sources_for_providers()
}

pub fn provider_secret_sources_for_providers(
    providers: &[AgentTaskExecutorProvider],
) -> HashMap<String, defaults::AgentTaskSecretSource> {
    let mut sources = HashMap::new();
    for provider in providers {
        sources.extend(provider_secret_sources(provider, None));
        for defaults in provider.provider_defaults.values() {
            sources.extend(provider_config_secret_sources(defaults));
        }
    }
    sources
}

/// Secret sources scoped to a single backend (and optional provider selector).
///
/// Mirrors the backend/selector resolution `agent-task doctor` uses so auth
/// status reports readiness for the exact backend cook/dispatch would target.
/// When `selector` is `None`, all providers for `backend` are included.
pub fn provider_secret_sources_for_backend(
    providers: &[AgentTaskExecutorProvider],
    backend: &str,
    selector: Option<&str>,
) -> HashMap<String, defaults::AgentTaskSecretSource> {
    let scoped: Vec<&AgentTaskExecutorProvider> = providers
        .iter()
        .filter(|provider| provider.backend == backend)
        .filter(|provider| selector.is_none_or(|selector| provider.id == selector))
        .collect();
    let mut sources = HashMap::new();
    for provider in scoped {
        sources.extend(provider_secret_sources(provider, None));
        for defaults in provider.provider_defaults.values() {
            sources.extend(provider_config_secret_sources(defaults));
        }
    }
    sources
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ProviderCommandEnvError {
    Secret(AgentTaskSecretResolutionError),
    Executable(AgentTaskProviderExecutableResolutionError),
}

fn provider_executable_env(
    provider: &AgentTaskExecutorProvider,
) -> Result<Vec<(String, String)>, AgentTaskProviderExecutableResolutionError> {
    let mut env = Vec::new();
    for readiness in &provider.runner_readiness {
        let Some(executable) = readiness.executable.as_ref() else {
            continue;
        };
        let resolved = resolve_provider_executable(readiness, executable)?;
        for name in resolved.env {
            env.push((name, resolved.path.clone()));
        }
    }
    Ok(env)
}

fn resolve_provider_executable(
    readiness: &AgentTaskProviderRunnerReadiness,
    executable: &AgentTaskProviderExecutableReadiness,
) -> Result<AgentTaskProviderResolvedExecutable, AgentTaskProviderExecutableResolutionError> {
    let env_names: Vec<String> = executable
        .env
        .iter()
        .filter(|name| !name.trim().is_empty())
        .cloned()
        .collect();
    for name in &env_names {
        if let Ok(value) = std::env::var(name) {
            let value = value.trim();
            if !value.is_empty() {
                return Ok(AgentTaskProviderResolvedExecutable {
                    env: env_names,
                    path: value.to_string(),
                });
            }
        }
    }

    for candidate in &executable.candidates {
        if let Some(path) = resolve_executable_candidate(candidate) {
            return Ok(AgentTaskProviderResolvedExecutable {
                env: env_names,
                path,
            });
        }
    }

    Err(AgentTaskProviderExecutableResolutionError {
        readiness_id: readiness.id.clone(),
        label: readiness.label.clone(),
        env: executable.env.clone(),
        candidates: executable.candidates.clone(),
        install_hint: executable
            .install_hint
            .clone()
            .or_else(|| readiness.remediation.clone()),
    })
}

fn resolve_executable_candidate(candidate: &str) -> Option<String> {
    let candidate = candidate.trim();
    if candidate.is_empty() {
        return None;
    }
    let candidate_path = Path::new(candidate);
    if candidate_path.components().count() > 1 || candidate_path.is_absolute() {
        return executable_file(candidate_path).then(|| candidate.to_string());
    }
    let path_var = std::env::var_os("PATH")?;
    for path in std::env::split_paths(&path_var) {
        let resolved = path.join(candidate);
        if executable_file(&resolved) {
            return Some(resolved.to_string_lossy().to_string());
        }
    }
    None
}

#[cfg(unix)]
fn executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    std::fs::metadata(path)
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn executable_file(path: &Path) -> bool {
    path.is_file()
}

pub(crate) fn role_aliases_for_executor(
    backend: &str,
    selector: Option<&str>,
) -> AgentTaskProviderRoleAliases {
    let catalog = AgentTaskProviderCatalog::discover();
    select_provider_by_backend(catalog.providers(), backend, selector)
        .map(|provider| provider.role_aliases.clone())
        .unwrap_or_default()
}

pub(crate) fn timeout_artifact_discovery_for_executor(
    backend: &str,
    selector: Option<&str>,
) -> AgentTaskProviderTimeoutArtifactDiscovery {
    let catalog = AgentTaskProviderCatalog::discover();
    select_provider_by_backend(catalog.providers(), backend, selector)
        .map(|provider| provider.timeout_artifact_discovery.clone())
        .unwrap_or_default()
}

pub(crate) fn role_aliases_for_provider(
    provider_id_or_backend: &str,
) -> AgentTaskProviderRoleAliases {
    let catalog = AgentTaskProviderCatalog::discover();
    catalog
        .providers()
        .iter()
        .find(|provider| {
            provider.id == provider_id_or_backend || provider.backend == provider_id_or_backend
        })
        .map(|provider| provider.role_aliases.clone())
        .unwrap_or_default()
}

fn provider_requires_cwd_git_checkout_with_providers(
    providers: &[AgentTaskExecutorProvider],
    backend: &str,
    selector: Option<&str>,
) -> bool {
    select_provider_by_backend(providers, backend, selector)
        .map(|provider| {
            provider.runtime_contract.apply_back.requires_git_checkout == Some(true)
                || provider.workspace_materialization.as_ref().is_some_and(
                    AgentTaskProviderWorkspaceMaterialization::requires_cwd_git_checkout,
                )
        })
        .unwrap_or(false)
}

fn apply_provider_runner_secret_env_contracts_with_providers(
    plan: &mut AgentTaskPlan,
    providers: &[AgentTaskExecutorProvider],
) {
    for request in &mut plan.tasks {
        let Some(provider) = select_provider(providers, request) else {
            continue;
        };
        request.executor.secret_env =
            provider_secret_env_plan(provider, request).secret_env_names();
    }
}

pub(crate) fn provider_runner_secret_env_for_plan_with_providers(
    plan: &AgentTaskPlan,
    providers: &[AgentTaskExecutorProvider],
) -> Vec<String> {
    let mut names = Vec::new();
    for request in &plan.tasks {
        let Some(provider) = select_provider(providers, request) else {
            continue;
        };
        names.extend(provider_secret_env_plan(provider, request).secret_env_names());
    }
    names.sort();
    names.dedup();
    names
}

pub(crate) fn provider_secret_sources_for_plan_with_providers(
    plan: &AgentTaskPlan,
    providers: &[AgentTaskExecutorProvider],
) -> HashMap<String, defaults::AgentTaskSecretSource> {
    let mut sources = HashMap::new();
    for request in &plan.tasks {
        let Some(provider) = select_provider(providers, request) else {
            continue;
        };
        sources.extend(provider_secret_sources(provider, Some(request)));
    }
    sources
}

fn provider_secret_env(
    provider: &AgentTaskExecutorProvider,
    request: Option<&AgentTaskRequest>,
) -> Vec<String> {
    let mut names = Vec::new();
    for readiness in &provider.runner_readiness {
        names.extend(readiness.secret_env.iter().cloned());
    }
    for requirement in &provider.secret_requirements {
        if requirement.required == Some(false) {
            continue;
        }
        if let Some(name) = &requirement.name {
            names.push(name.clone());
        }
        names.extend(requirement.env.iter().cloned());
    }
    for requirement in &provider.secret_env_requirements {
        if requirement_matches_request(requirement.when.as_ref(), request) {
            names.extend(requirement.env.iter().cloned());
        }
    }
    if let Some(request) = request {
        if let Some(provider_name) = request
            .executor
            .config
            .get("provider")
            .and_then(Value::as_str)
        {
            if let Some(defaults) = provider.provider_defaults.get(provider_name) {
                names.extend(provider_config_secret_env(defaults));
            }
        }
    }
    names.sort();
    names.dedup();
    names
}

fn provider_secret_env_plan(
    provider: &AgentTaskExecutorProvider,
    request: &AgentTaskRequest,
) -> SecretEnvPlan {
    let provider_names = provider_secret_env(provider, Some(request));
    let mut plan = SecretEnvPlan::from_secret_env_names(request.executor.secret_env.clone());
    plan.extend_secret_env_names(provider_names.clone());
    plan.map_env_names(provider.id.clone(), provider_names);
    plan
}

fn provider_secret_env_plan_with_status(
    provider: &AgentTaskExecutorProvider,
    request: &AgentTaskRequest,
) -> SecretEnvPlan {
    let plan = provider_secret_env_plan(provider, request);
    let status = secret_env_status_with_fallbacks(
        &plan.secret_env_names(),
        &provider_secret_sources(provider, Some(request)),
    )
    .into_iter()
    .map(|status| SecretEnvStatus {
        name: status.name,
        configured: status.configured,
        source: status.source,
    });
    plan.with_status(status).redacted()
}

fn provider_secret_sources(
    provider: &AgentTaskExecutorProvider,
    request: Option<&AgentTaskRequest>,
) -> HashMap<String, defaults::AgentTaskSecretSource> {
    let mut sources = HashMap::new();
    for requirement in &provider.secret_env_requirements {
        if requirement_matches_request(requirement.when.as_ref(), request) {
            sources.extend(secret_source_map_from_extra(&requirement.extra));
        }
    }
    if let Some(request) = request {
        if let Some(provider_name) = request
            .executor
            .config
            .get("provider")
            .and_then(Value::as_str)
        {
            if let Some(defaults) = provider.provider_defaults.get(provider_name) {
                sources.extend(provider_config_secret_sources(defaults));
            }
        }
    }
    sources
}

fn secret_source_map_from_extra(
    extra: &BTreeMap<String, Value>,
) -> HashMap<String, defaults::AgentTaskSecretSource> {
    for key in [
        "secret_env_sources",
        "secretEnvSources",
        "credential_sources",
        "credentialSources",
    ] {
        if let Some(value) = extra.get(key) {
            return secret_source_map(value);
        }
    }
    HashMap::new()
}

fn provider_config_secret_sources(
    config: &Value,
) -> HashMap<String, defaults::AgentTaskSecretSource> {
    let Some(config) = config.as_object() else {
        return HashMap::new();
    };
    for key in [
        "secret_env_sources",
        "secretEnvSources",
        "credential_sources",
        "credentialSources",
    ] {
        if let Some(value) = config.get(key) {
            return secret_source_map(value);
        }
    }
    HashMap::new()
}

fn secret_source_map(value: &Value) -> HashMap<String, defaults::AgentTaskSecretSource> {
    let Some(entries) = value.as_object() else {
        return HashMap::new();
    };
    entries
        .iter()
        .filter_map(|(name, source)| {
            serde_json::from_value::<defaults::AgentTaskSecretSource>(source.clone())
                .ok()
                .map(|source| (name.clone(), source))
        })
        .collect()
}

fn provider_config_secret_env(config: &Value) -> Vec<String> {
    let Some(config) = config.as_object() else {
        return Vec::new();
    };
    let mut names = Vec::new();
    for key in ["secret_env", "secretEnv"] {
        match config.get(key) {
            Some(Value::String(name)) => names.push(name.clone()),
            Some(Value::Array(items)) => names.extend(
                items
                    .iter()
                    .filter_map(|item| item.as_str().map(str::to_string)),
            ),
            _ => {}
        }
    }
    names
}

fn requirement_matches_request(when: Option<&Value>, request: Option<&AgentTaskRequest>) -> bool {
    let Some(when) = when else {
        return true;
    };
    let Some(request) = request else {
        return false;
    };
    let Ok(request_value) = serde_json::to_value(request) else {
        return false;
    };
    condition_matches(when, &request_value)
}

fn condition_matches(condition: &Value, request: &Value) -> bool {
    if let Some(any) = condition.get("any").and_then(Value::as_array) {
        return any.iter().any(|item| condition_matches(item, request));
    }
    if let Some(all) = condition.get("all").and_then(Value::as_array) {
        return all.iter().all(|item| condition_matches(item, request));
    }
    let Some(path) = condition.get("path").and_then(Value::as_str) else {
        return false;
    };
    let actual = value_at_contract_path(request, path);
    match condition.get("equals") {
        Some(expected) => actual == Some(expected),
        None => actual.is_some(),
    }
}

fn value_at_contract_path<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    if path == "provider" {
        return value_at_contract_path(value, "executor.config.provider");
    }
    let mut current = value;
    for part in path.split('.') {
        current = current.get(part)?;
    }
    Some(current)
}

fn default_backend_from_policy(component_id: Option<&str>) -> crate::core::Result<Option<String>> {
    if let Some(component_id) = component_id {
        if let Ok(component) = component::load(component_id) {
            if let Some(default_backend) = component_default_backend(&component) {
                return Ok(Some(default_backend));
            }
        }
    }

    let extension_defaults: Vec<String> = extension::load_all_extensions()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|manifest| {
            manifest
                .agent_task
                .and_then(|agent_task| agent_task.default_backend)
        })
        .filter(|backend| !backend.trim().is_empty())
        .collect();

    if extension_defaults.len() > 1 {
        return Err(Error::validation_invalid_argument(
            "backend",
            "agent-task default backend is ambiguous because multiple extension policies declare agent_task.default_backend",
            None,
            Some(vec![
                "Set /agent_task/default_backend in Homeboy config or pass --backend explicitly.".to_string(),
            ]),
        ));
    }
    if let Some(default_backend) = extension_defaults.into_iter().next() {
        return Ok(Some(default_backend));
    }

    Ok(defaults::load_config()
        .agent_task
        .default_backend
        .filter(|backend| !backend.trim().is_empty()))
}

fn component_default_backend(component: &component::Component) -> Option<String> {
    component
        .extensions
        .as_ref()?
        .values()
        .find_map(|extension| {
            extension
                .settings
                .get("agent_task")
                .and_then(|value| value.get("default_backend"))
                .and_then(Value::as_str)
                .or_else(|| {
                    extension
                        .settings
                        .get("agent_task_default_backend")
                        .and_then(Value::as_str)
                })
                .filter(|backend| !backend.trim().is_empty())
                .map(String::from)
        })
}

impl AgentTaskExecutorAdapter for ExtensionProviderAgentTaskExecutor {
    fn execute(
        &self,
        request: AgentTaskRequest,
        _context: AgentTaskExecutionContext,
    ) -> AgentTaskOutcome {
        if request.executor.backend == "fixture" {
            return run_fixture_provider(&request);
        }
        if is_repo_local_gate_request(&request) {
            return run_repo_local_gate_task(&request);
        }

        let provider = match resolve_provider_for_backend(
            &self.providers,
            &request.executor.backend,
            request.executor.selector.as_deref(),
        ) {
            ProviderResolution::Resolved(provider) => provider,
            resolution => return provider_resolution_failure_outcome(&request, resolution),
        };

        let missing_capabilities: Vec<String> = request
            .executor
            .required_capabilities
            .iter()
            .filter(|capability| !provider.capabilities.contains(capability))
            .cloned()
            .collect();
        if !missing_capabilities.is_empty() {
            return failure_outcome(
                &request,
                AgentTaskOutcomeStatus::Failed,
                AgentTaskFailureClassification::CapabilityMissing,
                "agent_task.capability_missing",
                format!(
                    "provider '{}' is missing required capabilities: {}",
                    provider.id,
                    missing_capabilities.join(", ")
                ),
                json!({ "provider": provider.id, "missing_capabilities": missing_capabilities }),
            );
        }

        run_provider_command(&request, provider)
    }
}

fn provider_resolution_failure_outcome(
    request: &AgentTaskRequest,
    resolution: ProviderResolution<'_>,
) -> AgentTaskOutcome {
    match resolution {
        ProviderResolution::Resolved(_) => unreachable!("resolved provider handled before failure"),
        ProviderResolution::NotFound => failure_outcome(
            request,
            AgentTaskOutcomeStatus::Failed,
            AgentTaskFailureClassification::CapabilityMissing,
            "agent_task.provider_missing",
            format!(
                "no extension agent-task provider found for backend '{}'",
                request.executor.backend
            ),
            json!({ "backend": request.executor.backend }),
        ),
        ProviderResolution::AmbiguousExtensionAlias { candidate_ids } => failure_outcome(
            request,
            AgentTaskOutcomeStatus::Failed,
            AgentTaskFailureClassification::CapabilityMissing,
            "agent_task.provider_ambiguous",
            format!(
                "multiple extension agent-task providers match backend '{}'; pass --selector with one provider id",
                request.executor.backend
            ),
            json!({
                "backend": request.executor.backend,
                "available_provider_ids": candidate_ids,
            }),
        ),
        ProviderResolution::SelectorMismatch { available_ids } => failure_outcome(
            request,
            AgentTaskOutcomeStatus::Failed,
            AgentTaskFailureClassification::CapabilityMissing,
            "agent_task.provider_selector_mismatch",
            format!(
                "no extension agent-task provider for backend '{}' matched selector '{}'",
                request.executor.backend,
                request.executor.selector.as_deref().unwrap_or("")
            ),
            json!({
                "backend": request.executor.backend,
                "selector": request.executor.selector,
                "available_provider_ids": available_ids,
            }),
        ),
    }
}

fn run_fixture_provider(request: &AgentTaskRequest) -> AgentTaskOutcome {
    let mode = request
        .executor
        .config
        .get("mode")
        .and_then(Value::as_str)
        .unwrap_or("success");
    let artifact_root = fixture_artifact_root(request);
    if let Err(error) = std::fs::create_dir_all(&artifact_root) {
        return failure_outcome(
            request,
            AgentTaskOutcomeStatus::ProviderError,
            AgentTaskFailureClassification::Provider,
            "agent_task.fixture_artifact_root_failed",
            error.to_string(),
            json!({ "artifact_root": artifact_root.display().to_string() }),
        );
    }

    match mode {
        "empty_patch" => fixture_empty_patch_outcome(request, &artifact_root),
        "empty_runtime_bundle" => fixture_empty_runtime_bundle_outcome(request, &artifact_root),
        _ => fixture_success_outcome(request, &artifact_root),
    }
}

fn fixture_artifact_root(request: &AgentTaskRequest) -> PathBuf {
    request
        .executor
        .config
        .get("artifact_root")
        .and_then(Value::as_str)
        .map(|path| {
            let path = PathBuf::from(path);
            if path.is_absolute() {
                path
            } else {
                std::env::current_dir()
                    .unwrap_or_else(|_| PathBuf::from("."))
                    .join(path)
            }
        })
        .unwrap_or_else(|| {
            std::env::temp_dir().join(format!("homeboy-agent-task-fixture-{}", request.task_id))
        })
}

fn fixture_success_outcome(request: &AgentTaskRequest, artifact_root: &Path) -> AgentTaskOutcome {
    let changed_file = request
        .executor
        .config
        .get("changed_file")
        .and_then(Value::as_str)
        .unwrap_or("docs/agent-task-smoke.md");
    let patch_path = artifact_root.join("changes.patch");
    let transcript_path = artifact_root.join("transcript.log");
    let result_path = artifact_root.join("agent-result.json");
    let patch = format!(
        "diff --git a/{changed_file} b/{changed_file}\n--- a/{changed_file}\n+++ b/{changed_file}\n@@ -1 +1 @@\n-before\n+after\n"
    );
    let _ = std::fs::write(&patch_path, patch);
    let _ = std::fs::write(
        &transcript_path,
        "fixture provider completed successfully\n",
    );

    let metadata = request
        .executor
        .config
        .get("metadata")
        .cloned()
        .unwrap_or_else(|| json!({ "fixture_mode": "success" }));
    let mut outcome = AgentTaskOutcome {
        schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
        task_id: request.task_id.clone(),
        status: AgentTaskOutcomeStatus::Succeeded,
        summary: Some("fixture provider wrote deterministic smoke artifacts".to_string()),
        failure_classification: None,
        artifacts: vec![
            fixture_artifact("patch", "patch", &patch_path, Some("text/x-patch")),
            fixture_artifact(
                "agent-result",
                "agent_result",
                &result_path,
                Some("application/json"),
            ),
        ],
        typed_artifacts: Vec::new(),
        evidence_refs: vec![AgentTaskEvidenceRef {
            kind: "transcript".to_string(),
            uri: transcript_path.display().to_string(),
            label: Some("fixture transcript".to_string()),
        }],
        diagnostics: vec![AgentTaskDiagnostic {
            class: "agent_task.fixture_success".to_string(),
            message: "fixture provider generated patch, transcript, and result artifacts"
                .to_string(),
            data: json!({ "artifact_root": artifact_root.display().to_string() }),
        }],
        outputs: Value::Null,
        workflow: None,
        follow_up: None,
        metadata,
    };
    let _ = std::fs::write(
        &result_path,
        serde_json::to_string_pretty(&outcome).unwrap_or_else(|_| "{}".to_string()),
    );
    outcome.artifacts[1] = fixture_artifact(
        "agent-result",
        "agent_result",
        &result_path,
        Some("application/json"),
    );
    outcome
}

fn fixture_empty_patch_outcome(
    request: &AgentTaskRequest,
    artifact_root: &Path,
) -> AgentTaskOutcome {
    let patch_path = artifact_root.join("empty.patch");
    let _ = std::fs::write(&patch_path, "");
    failure_outcome(
        request,
        AgentTaskOutcomeStatus::NoOp,
        AgentTaskFailureClassification::InvalidInput,
        "agent_task.fixture_empty_patch",
        "fixture produced an empty patch; promotion should reject it".to_string(),
        json!({ "patch_path": patch_path.display().to_string() }),
    )
}

fn fixture_empty_runtime_bundle_outcome(
    request: &AgentTaskRequest,
    artifact_root: &Path,
) -> AgentTaskOutcome {
    let bundle_path = artifact_root.join("runtime-bundle");
    let _ = std::fs::create_dir_all(&bundle_path);
    let mut outcome = failure_outcome(
        request,
        AgentTaskOutcomeStatus::ProviderError,
        AgentTaskFailureClassification::Provider,
        "agent_task.fixture_empty_runtime_bundle",
        "fixture produced an empty runtime bundle".to_string(),
        json!({ "bundle_path": bundle_path.display().to_string() }),
    );
    outcome.artifacts.push(fixture_artifact(
        "runtime-bundle",
        "runtime_bundle",
        &bundle_path,
        None,
    ));
    outcome
}

fn fixture_artifact(id: &str, kind: &str, path: &PathBuf, mime: Option<&str>) -> AgentTaskArtifact {
    AgentTaskArtifact {
        schema: AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
        id: id.to_string(),
        kind: kind.to_string(),
        name: path
            .file_name()
            .map(|name| name.to_string_lossy().to_string()),
        label: None,
        role: None,
        semantic_key: None,
        path: Some(path.display().to_string()),
        url: None,
        mime: mime.map(str::to_string),
        size_bytes: std::fs::metadata(path).ok().map(|metadata| metadata.len()),
        sha256: None,
        metadata: json!({ "fixture": true }),
    }
}

fn discover_agent_task_executor_providers() -> Vec<AgentTaskExecutorProvider> {
    agent_runtime_manifest::discover_agent_task_executor_providers()
}

fn select_provider<'a>(
    providers: &'a [AgentTaskExecutorProvider],
    request: &AgentTaskRequest,
) -> Option<&'a AgentTaskExecutorProvider> {
    select_provider_by_backend(
        providers,
        &request.executor.backend,
        request.executor.selector.as_deref(),
    )
}

/// Structured outcome of resolving a `--backend`/`--selector` request against a
/// concrete provider list. This is the single source of truth shared by every
/// caller that asks "can this backend/selector run here?" — execution-time
/// selection, the local availability check, and the Lab runner preflight. By
/// returning a typed reason (rather than a bare `Option`/`bool`) the preflight
/// can explain *why* a provider that `agent-task providers` lists is still not
/// selectable, instead of emitting a misleading "availability is false".
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ProviderResolution<'a> {
    /// Exactly one provider matched the backend/selector.
    Resolved(&'a AgentTaskExecutorProvider),
    /// No provider matched the backend either exactly or via extension alias.
    NotFound,
    /// Multiple providers share `extension_id == backend` and no selector
    /// disambiguated them, so the alias is ambiguous. The candidate provider
    /// ids are surfaced so callers can tell the operator which `--selector`
    /// values would resolve it.
    AmbiguousExtensionAlias { candidate_ids: Vec<String> },
    /// One or more providers matched the backend/extension alias, but the
    /// supplied selector did not match any of them. The selectable provider
    /// ids are surfaced so the operator can correct the `--selector`.
    SelectorMismatch { available_ids: Vec<String> },
}

impl<'a> ProviderResolution<'a> {
    pub(crate) fn resolved(self) -> Option<&'a AgentTaskExecutorProvider> {
        match self {
            ProviderResolution::Resolved(provider) => Some(provider),
            _ => None,
        }
    }
}

/// Resolve a backend/selector request against a provider list, returning a
/// structured outcome. This is the shared resolution contract; execution-time
/// `select_provider`, the local availability check, and the Lab preflight all
/// funnel through here so they can never disagree about the same provider list.
pub(crate) fn resolve_provider_for_backend<'a>(
    providers: &'a [AgentTaskExecutorProvider],
    backend: &str,
    selector: Option<&str>,
) -> ProviderResolution<'a> {
    let exact_matches: Vec<&AgentTaskExecutorProvider> = providers
        .iter()
        .filter(|provider| provider.backend == backend)
        .collect();

    if !exact_matches.is_empty() {
        if let Some(provider) = exact_matches
            .iter()
            .find(|provider| selector.is_none_or(|selector| provider.id == selector))
        {
            return ProviderResolution::Resolved(provider);
        }
        return ProviderResolution::SelectorMismatch {
            available_ids: exact_matches
                .iter()
                .map(|provider| provider.id.clone())
                .collect(),
        };
    }
    resolve_provider_by_extension_alias(providers, backend, selector)
}

fn select_provider_by_backend<'a>(
    providers: &'a [AgentTaskExecutorProvider],
    backend: &str,
    selector: Option<&str>,
) -> Option<&'a AgentTaskExecutorProvider> {
    resolve_provider_for_backend(providers, backend, selector).resolved()
}

fn resolve_provider_by_extension_alias<'a>(
    providers: &'a [AgentTaskExecutorProvider],
    backend: &str,
    selector: Option<&str>,
) -> ProviderResolution<'a> {
    let alias_matches: Vec<&AgentTaskExecutorProvider> = providers
        .iter()
        .filter(|provider| provider.extension_id.as_deref() == Some(backend))
        .collect();

    if alias_matches.is_empty() {
        return ProviderResolution::NotFound;
    }

    match selector {
        None => {
            if alias_matches.len() == 1 {
                ProviderResolution::Resolved(alias_matches[0])
            } else {
                ProviderResolution::AmbiguousExtensionAlias {
                    candidate_ids: alias_matches
                        .iter()
                        .map(|provider| provider.id.clone())
                        .collect(),
                }
            }
        }
        Some(selector) => match alias_matches
            .iter()
            .find(|provider| provider.id == selector)
        {
            Some(provider) => ProviderResolution::Resolved(provider),
            None => ProviderResolution::SelectorMismatch {
                available_ids: alias_matches
                    .iter()
                    .map(|provider| provider.id.clone())
                    .collect(),
            },
        },
    }
}

fn required_extension_ids_for_plan_with_providers(
    plan: &AgentTaskPlan,
    providers: &[AgentTaskExecutorProvider],
) -> Vec<String> {
    let mut extension_ids = BTreeSet::new();
    for request in &plan.tasks {
        if let Some(extension_id) = select_provider(providers, request)
            .and_then(|provider| provider.extension_id.as_ref())
            .filter(|extension_id| !extension_id.trim().is_empty())
        {
            extension_ids.insert(extension_id.clone());
        }
    }
    extension_ids.into_iter().collect()
}

/// Maximum number of attempts (1 initial + retries) for a transient provider
/// or network failure. Mirrors the bounded-retry pattern already used for
/// transient SSH failures (`server::client`) and SQLite-lock contention
/// (`observation::store`).
const PROVIDER_TRANSIENT_MAX_ATTEMPTS: u32 = 3;

/// Base backoff between transient retries; doubles each attempt
/// (250ms, 500ms, ...). Keeps a single network blip from failing a whole cook
/// task without introducing unbounded delay.
const PROVIDER_TRANSIENT_BASE_BACKOFF_MS: u64 = 250;

/// Run the provider command with a bounded retry on transient provider/network
/// failures.
///
/// Transient failures (timeouts, connection resets, cURL error 28, 5xx,
/// temporarily-unavailable) are classified as retryable and retried with
/// escalating backoff. Permanent failures (auth, validation, malformed input,
/// capability gaps) fail fast on the first attempt. Each retry is surfaced in
/// the returned outcome diagnostics so the behaviour is visible in run output.
fn run_provider_command(
    request: &AgentTaskRequest,
    provider: &AgentTaskExecutorProvider,
) -> AgentTaskOutcome {
    let mut attempt = 1;
    loop {
        let mut outcome = run_provider_command_once(request, provider);
        classify_transient_provider_outcome(&mut outcome);

        let retryable = outcome_is_transient(&outcome);
        if !retryable || attempt >= PROVIDER_TRANSIENT_MAX_ATTEMPTS {
            if attempt > 1 {
                annotate_transient_retry(&mut outcome, attempt, retryable);
            }
            return outcome;
        }

        let backoff_ms = PROVIDER_TRANSIENT_BASE_BACKOFF_MS.saturating_mul(1u64 << (attempt - 1));
        if backoff_ms > 0 {
            std::thread::sleep(Duration::from_millis(backoff_ms));
        }
        attempt += 1;
    }
}

/// True when an outcome represents a transient provider/network failure that is
/// safe to retry.
fn outcome_is_transient(outcome: &AgentTaskOutcome) -> bool {
    outcome.failure_classification == Some(AgentTaskFailureClassification::Transient)
}

/// Promote a `ProviderError`/`Provider` outcome to the `Transient`
/// classification when its surfaced text looks like a transient network or
/// provider blip. Leaves permanent provider failures untouched so they keep
/// failing fast.
fn classify_transient_provider_outcome(outcome: &mut AgentTaskOutcome) {
    let already_transient =
        outcome.failure_classification == Some(AgentTaskFailureClassification::Transient);
    let provider_failure = matches!(
        outcome.status,
        AgentTaskOutcomeStatus::ProviderError | AgentTaskOutcomeStatus::Failed
    ) && matches!(
        outcome.failure_classification,
        Some(AgentTaskFailureClassification::Provider) | None
    );

    if already_transient {
        return;
    }
    if !provider_failure {
        return;
    }

    if outcome_text_is_transient(outcome) {
        outcome.failure_classification = Some(AgentTaskFailureClassification::Transient);
    }
}

/// Gather the human-facing text of an outcome (summary, diagnostic messages,
/// diagnostic data) and check it for transient-failure signatures.
fn outcome_text_is_transient(outcome: &AgentTaskOutcome) -> bool {
    if let Some(summary) = outcome.summary.as_deref() {
        if is_transient_provider_error(summary) {
            return true;
        }
    }
    for diagnostic in &outcome.diagnostics {
        if is_transient_provider_error(&diagnostic.message) {
            return true;
        }
        if is_transient_provider_error(&diagnostic.data.to_string()) {
            return true;
        }
    }
    false
}

/// Classify provider/network error text as transient (retryable) vs permanent.
///
/// Mirrors `server::client::is_transient_ssh_error`: matches on a curated set
/// of substrings that indicate a transient blip rather than a deterministic
/// failure. Matching is case-insensitive.
fn is_transient_provider_error(text: &str) -> bool {
    let lowered = text.to_ascii_lowercase();
    const TRANSIENT_PATTERNS: [&str; 16] = [
        "curl error 28",
        "operation timed out",
        "timed out",
        "timeout",
        "connection reset",
        "connection refused",
        "connection closed",
        "broken pipe",
        "network error",
        "network is unreachable",
        "temporary failure",
        "temporarily unavailable",
        "service unavailable",
        "bad gateway",
        "gateway timeout",
        "too many requests",
    ];

    if TRANSIENT_PATTERNS
        .iter()
        .any(|pattern| lowered.contains(pattern))
    {
        return true;
    }

    // HTTP 5xx and 429 status codes are transient; 4xx (except 429) are not.
    transient_status_code(&lowered)
}

/// Detect a transient HTTP status code (5xx or 429) mentioned in error text,
/// while leaving permanent 4xx codes (400/401/403/404/422) non-retryable.
fn transient_status_code(lowered: &str) -> bool {
    const TRANSIENT_CODES: [&str; 7] = ["429", "500", "502", "503", "504", "522", "524"];
    TRANSIENT_CODES
        .iter()
        .any(|code| contains_status_code_token(lowered, code))
}

fn contains_status_code_token(text: &str, code: &str) -> bool {
    text.match_indices(code).any(|(index, _)| {
        let before = text[..index].chars().next_back();
        let after = text[index + code.len()..].chars().next();
        !before.is_some_and(|ch| ch.is_ascii_alphanumeric())
            && !after.is_some_and(|ch| ch.is_ascii_alphanumeric())
    })
}

/// Record the transient retry history on the final outcome so operators can see
/// that a cook task recovered from (or exhausted retries on) a transient blip.
fn annotate_transient_retry(outcome: &mut AgentTaskOutcome, attempts: u32, exhausted: bool) {
    let message = if exhausted {
        format!(
            "transient provider/network failure persisted after {attempts} attempt(s); retries exhausted"
        )
    } else {
        format!(
            "recovered after retrying transient provider/network failure ({attempts} attempt(s))"
        )
    };
    outcome.diagnostics.push(AgentTaskDiagnostic {
        class: "agent_task.provider_transient_retry".to_string(),
        message,
        data: json!({ "attempts": attempts, "retries_exhausted": exhausted }),
    });
}

fn run_provider_command_once(
    request: &AgentTaskRequest,
    provider: &AgentTaskExecutorProvider,
) -> AgentTaskOutcome {
    let command = render_provider_command_display(provider);
    let Some((program, args, cwd)) = provider_command_parts(provider) else {
        return failure_outcome(
            request,
            AgentTaskOutcomeStatus::ProviderError,
            AgentTaskFailureClassification::Provider,
            "agent_task.provider_command_empty",
            format!("provider '{}' has an empty command", provider.id),
            json!({ "provider": provider.id }),
        );
    };

    if let Some(preflight) = provider_preflight_failure(request, provider, &program, &cwd, &command)
    {
        return preflight;
    }

    let timeout = request
        .limits
        .timeout_ms
        .or(request.limits.max_runtime_ms)
        .map(|timeout_ms| (timeout_ms, timeout_with_grace(timeout_ms)));
    let mut provider_request = request.clone();
    provider_request.normalize_artifact_declarations();
    let input = match serde_json::to_vec(&provider_request) {
        Ok(input) => input,
        Err(error) => {
            return failure_outcome(
                request,
                AgentTaskOutcomeStatus::Failed,
                AgentTaskFailureClassification::InvalidInput,
                "agent_task.request_encode_failed",
                error.to_string(),
                json!({ "provider": provider.id }),
            )
        }
    };
    let env = match provider_command_env(request, provider) {
        Ok(env) => env,
        Err(ProviderCommandEnvError::Secret(error)) => {
            return failure_outcome(
                request,
                AgentTaskOutcomeStatus::ProviderError,
                AgentTaskFailureClassification::InvalidInput,
                "agent_task.secret_env_missing",
                error.message,
                json!({ "provider": provider.id, "missing_secret_env": error.missing_secret_env }),
            )
        }
        Err(ProviderCommandEnvError::Executable(error)) => {
            return failure_outcome(
                request,
                AgentTaskOutcomeStatus::ProviderError,
                AgentTaskFailureClassification::Provider,
                "agent_task.provider_executable_missing",
                error.message(),
                json!({
                    "provider": provider.id,
                    "readiness_id": error.readiness_id,
                    "env": error.env,
                    "candidates": error.candidates,
                    "install_hint": error.install_hint,
                }),
            )
        }
    };

    let mut command_builder = Command::new(&program);
    command_builder.args(&args).envs(
        env.iter()
            .map(|(key, value)| (key.as_str(), value.as_str())),
    );
    if let Some(cwd) = cwd {
        command_builder.current_dir(cwd);
    }

    let mut child = match command_builder
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            return failure_outcome(
                request,
                AgentTaskOutcomeStatus::ProviderError,
                AgentTaskFailureClassification::Provider,
                "agent_task.provider_spawn_failed",
                error.to_string(),
                json!({ "provider": provider.id, "command": command }),
            )
        }
    };

    if let Some(mut stdin) = child.stdin.take() {
        let _ = std::io::Write::write_all(&mut stdin, &input);
    }

    let output = if let Some((requested_timeout_ms, process_timeout)) = timeout {
        let started = Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(_status)) => break child.wait_with_output(),
                Ok(None) if started.elapsed() >= process_timeout => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return failure_outcome(
                        request,
                        AgentTaskOutcomeStatus::Timeout,
                        AgentTaskFailureClassification::Timeout,
                        "agent_task.provider_timeout",
                        format!(
                            "provider '{}' exceeded timeout_ms={}",
                            provider.id, requested_timeout_ms
                        ),
                        json!({ "provider": provider.id, "command": command, "timeout_ms": requested_timeout_ms, "process_timeout_ms": process_timeout.as_millis() }),
                    );
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(10)),
                Err(error) => break Err(error),
            }
        }
    } else {
        child.wait_with_output()
    };

    let Ok(output) = output else {
        return failure_outcome(
            request,
            AgentTaskOutcomeStatus::ProviderError,
            AgentTaskFailureClassification::Provider,
            "agent_task.provider_io_failed",
            "provider command failed while collecting output".to_string(),
            json!({ "provider": provider.id, "command": command }),
        );
    };

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if stdout.is_empty() {
        if let Some(mut outcome) = parse_provider_outcome_from_mixed_output(&stderr) {
            normalize_provider_outcome_roles(&mut outcome, provider);
            return outcome;
        }
        return failure_outcome(
            request,
            AgentTaskOutcomeStatus::ProviderError,
            AgentTaskFailureClassification::Provider,
            "agent_task.provider_empty_stdout",
            format!("provider '{}' produced no JSON outcome", provider.id),
            json!({ "provider": provider.id, "command": command, "exit_code": output.status.code(), "stderr": stderr }),
        );
    }

    let parsed: Result<AgentTaskOutcome, _> = serde_json::from_str(&stdout);
    match parsed {
        Ok(mut outcome) => {
            if outcome.schema != AGENT_TASK_OUTCOME_SCHEMA {
                outcome.schema = AGENT_TASK_OUTCOME_SCHEMA.to_string();
            }
            normalize_provider_outcome_roles(&mut outcome, provider);
            outcome
        }
        Err(error) => failure_outcome(
            request,
            AgentTaskOutcomeStatus::ProviderError,
            AgentTaskFailureClassification::Provider,
            "agent_task.provider_malformed_json",
            format!(
                "provider '{}' returned malformed JSON: {error}",
                provider.id
            ),
            json!({ "provider": provider.id, "command": command, "exit_code": output.status.code(), "stderr": stderr, "stdout": stdout }),
        ),
    }
}

fn provider_preflight_failure(
    request: &AgentTaskRequest,
    provider: &AgentTaskExecutorProvider,
    program: &str,
    cwd: &Option<PathBuf>,
    command: &str,
) -> Option<AgentTaskOutcome> {
    let digest = provider_preflight_digest(request, provider, program, cwd, command);
    if digest.failures.is_empty() {
        return None;
    }

    Some(failure_outcome(
        request,
        AgentTaskOutcomeStatus::ProviderError,
        digest.classification,
        digest.diagnostic_class,
        digest.message,
        digest.data,
    ))
}

struct ProviderPreflightDigest {
    diagnostic_class: &'static str,
    classification: AgentTaskFailureClassification,
    message: String,
    data: Value,
    failures: Vec<Value>,
}

fn provider_preflight_digest(
    request: &AgentTaskRequest,
    provider: &AgentTaskExecutorProvider,
    program: &str,
    cwd: &Option<PathBuf>,
    command: &str,
) -> ProviderPreflightDigest {
    let mut failures = Vec::new();
    let mut diagnostic_class = "agent_task.provider_preflight_failed";
    let mut classification = AgentTaskFailureClassification::Provider;

    if !provider_command_program_available(program) {
        diagnostic_class = "agent_task.provider_command_unavailable";
        failures.push(json!({
            "field": "command",
            "message": format!("provider command executable '{program}' is not available"),
            "remediation": format!("Install '{program}' on the runner or configure the provider invocation with an absolute executable path available to the runner PATH."),
        }));
    }

    if let Some(cwd) = cwd {
        if !cwd.is_dir() {
            failures.push(json!({
                "field": "invocation.cwd",
                "message": format!("provider command working directory '{}' does not exist", cwd.display()),
                "remediation": "Fix the provider runtime path or invocation.cwd template so it resolves to an existing directory on the runner.",
            }));
        }
    }

    let secret_status = provider_secret_env_plan_with_status(provider, request).status;
    let missing_secret_env: Vec<String> = secret_status
        .iter()
        .filter(|status| !status.configured)
        .map(|status| status.name.clone())
        .collect();
    if !missing_secret_env.is_empty() {
        diagnostic_class = "agent_task.secret_env_missing";
        classification = AgentTaskFailureClassification::InvalidInput;
        failures.push(json!({
            "field": "secret_env",
            "message": format!("missing provider secret env: {}", missing_secret_env.join(", ")),
            "remediation": "Set the missing secret_env values in the runner environment or Homeboy secret-env configuration before launching the sandbox.",
        }));
    }

    let message = if failures.len() == 1 {
        failures[0]
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("agent-task provider preflight failed")
            .to_string()
    } else {
        format!(
            "agent-task provider preflight failed with {} actionable issue(s)",
            failures.len()
        )
    };

    let digest_failures = failures.clone();
    let data = json!({
        "provider": provider.id,
        "backend": provider.backend,
        "command": command,
        "program": program,
        "path": std::env::var_os("PATH").map(|value| value.to_string_lossy().to_string()).unwrap_or_default(),
        "runtime_path_provenance": runtime_path_provenance(provider),
        "missing_secret_env": missing_secret_env,
        "secret_env_status": secret_status,
        "failures": failures,
    });

    ProviderPreflightDigest {
        diagnostic_class,
        classification,
        message,
        data,
        failures: digest_failures,
    }
}

fn provider_command_program_available(program: &str) -> bool {
    let program = program.trim();
    if program.is_empty() {
        return false;
    }
    let path = Path::new(program);
    if path.components().count() > 1 || path.is_absolute() {
        return executable_file(path);
    }
    resolve_executable_candidate(program).is_some()
}

fn runtime_path_provenance(provider: &AgentTaskExecutorProvider) -> Value {
    let (path, source) = if let Some(runtime_path) = provider.runtime_path.as_deref() {
        (runtime_path, "runtime_path")
    } else if let Some(extension_path) = provider.extension_path.as_deref() {
        (extension_path, "extension_path_fallback")
    } else {
        ("", "missing")
    };
    json!({
        "runtime_id": provider.runtime_id.as_deref(),
        "runtime_path": path,
        "source": source,
        "extension_id": provider.extension_id.as_deref(),
        "extension_path": provider.extension_path.as_deref(),
    })
}

fn parse_provider_outcome_from_mixed_output(output: &str) -> Option<AgentTaskOutcome> {
    if output.trim().is_empty() {
        return None;
    }
    if let Ok(outcome) = serde_json::from_str::<AgentTaskOutcome>(output) {
        return Some(outcome);
    }

    for (index, _) in output.match_indices('{') {
        let mut stream =
            serde_json::Deserializer::from_str(&output[index..]).into_iter::<AgentTaskOutcome>();
        if let Some(Ok(outcome)) = stream.next() {
            return Some(outcome);
        }
    }
    None
}

fn normalize_provider_outcome_roles(
    outcome: &mut AgentTaskOutcome,
    provider: &AgentTaskExecutorProvider,
) {
    normalize_provider_artifact_roles(&mut outcome.artifacts, &provider.role_aliases);
    normalize_provider_run_result_output(outcome, &provider.role_aliases);
    normalize_provider_runtime_contract(outcome, provider);
    surface_provider_run_result_diagnostics(outcome);
}

/// Whether an outcome status represents a failure for which we want to mine the
/// provider run-result for actionable evidence.
fn is_failure_status(status: AgentTaskOutcomeStatus) -> bool {
    matches!(
        status,
        AgentTaskOutcomeStatus::Failed
            | AgentTaskOutcomeStatus::ProviderError
            | AgentTaskOutcomeStatus::Timeout
            | AgentTaskOutcomeStatus::UnableToRemediate
    )
}

/// Surface actionable provider run-result evidence into the outcome on failure
/// (#4105).
///
/// Provider executors emit a
/// structured run-result under `outputs.provider_run_result` following the
/// `*/agent-task-run-result/*` shape: `{status, failure_classification,
/// diagnostics[], artifacts[], metadata{provider_error,run_id,run_status,
/// runtime_id,runtime_status}, refs{logs,transcripts,artifact_bundles,...}}`.
///
/// Before this fix a FAILED run-result could be preserved verbatim while
/// homeboy surfaced nothing actionable — `agent-task logs/artifacts/review`
/// showed only the generic "agent task failed" summary even though the
/// run-result carried (or conspicuously lacked) provider error codes, a run /
/// runtime id, and log / transcript refs.
///
/// This walks the preserved run-result and ADDS (never overwrites) the
/// following to the outcome so operators get actionable info:
/// - each run-result `diagnostics[]` entry becomes an outcome diagnostic;
/// - `metadata.provider_error` + run/runtime ids + statuses become a single
///   `provider.run_result_failed` diagnostic and are mirrored onto
///   `outcome.metadata.provider_error`;
/// - `refs.{logs,transcripts,artifact_bundles,runtimes,patches}` become
///   `evidence_refs` so review/artifacts can surface them;
/// - if the run-result is an empty shell (no diagnostics, no provider_error, no
///   run/runtime id, no refs) a single reviewer-safe diagnostic explains that
///   no provider runtime/session was created, satisfying the acceptance rule
///   that a failed run-result is never an empty shell.
///
/// It is fully provider-agnostic: it keys only off the generic run-result shape
/// and never references any specific runtime, framework, or provider id.
fn surface_provider_run_result_diagnostics(outcome: &mut AgentTaskOutcome) {
    if !is_failure_status(outcome.status) {
        return;
    }
    let Some(run_result) = output_value(&outcome.outputs, "provider_run_result").cloned() else {
        return;
    };
    let Some(run_result) = run_result.as_object() else {
        return;
    };

    // Only mine FAILED (or non-succeeded) run-results. A run-result that omits
    // status is treated as a failure here because the outcome itself already
    // failed.
    let run_status = run_result
        .get("status")
        .and_then(Value::as_str)
        .map(str::to_string);
    if matches!(run_status.as_deref(), Some("succeeded") | Some("success")) {
        return;
    }

    let mut surfaced_evidence = false;

    // 1. Lift each run-result diagnostic into the outcome diagnostics, deduped
    //    by (class, message) against what is already present.
    if let Some(Value::Array(items)) = run_result.get("diagnostics") {
        for item in items {
            let Some(message) = item
                .get("message")
                .and_then(Value::as_str)
                .map(str::to_string)
            else {
                continue;
            };
            if message.trim().is_empty() {
                continue;
            }
            let class = item
                .get("class")
                .or_else(|| item.get("kind"))
                .or_else(|| item.get("code"))
                .and_then(Value::as_str)
                .unwrap_or("provider.run_result_diagnostic")
                .to_string();
            push_unique_diagnostic(
                &mut outcome.diagnostics,
                class,
                message,
                item.get("data").cloned().unwrap_or(Value::Null),
            );
            surfaced_evidence = true;
        }
    }

    // 2. Pull the structured failure metadata (provider error + run/runtime
    //    identity + statuses) into a single actionable diagnostic and mirror
    //    provider_error onto the outcome metadata.
    let metadata = run_result.get("metadata").and_then(Value::as_object);
    let provider_error = metadata
        .and_then(|map| map.get("provider_error"))
        .filter(|value| !is_empty_value(value))
        .cloned();
    let run_id = metadata
        .and_then(|map| map.get("run_id"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string);
    let runtime_id = metadata
        .and_then(|map| map.get("runtime_id"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string);
    let runtime_status = metadata
        .and_then(|map| map.get("runtime_status"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string);

    let has_identity = provider_error.is_some()
        || run_id.is_some()
        || runtime_id.is_some()
        || runtime_status.is_some();

    if has_identity {
        let message = describe_run_result_failure(
            run_status.as_deref(),
            run_id.as_deref(),
            runtime_id.as_deref(),
            runtime_status.as_deref(),
            provider_error.as_ref(),
        );
        let data = json!({
            "run_status": run_status,
            "run_id": run_id,
            "runtime_id": runtime_id,
            "runtime_status": runtime_status,
            "provider_error": provider_error,
        });
        push_unique_diagnostic(
            &mut outcome.diagnostics,
            "provider.run_result_failed".to_string(),
            message,
            data,
        );
        surfaced_evidence = true;

        if let Some(provider_error) = provider_error {
            mirror_provider_error_metadata(outcome, provider_error);
        }
    }

    // 3. Promote run-result refs (logs, transcripts, artifact bundles, runtimes,
    //    patches) into evidence refs so review/artifacts can surface them.
    if let Some(refs) = run_result.get("refs").and_then(Value::as_object) {
        for (group, kind) in [
            ("logs", "provider-log"),
            ("transcripts", "provider-transcript"),
            ("artifact_bundles", "provider-artifact-bundle"),
            ("runtimes", "provider-runtime"),
            ("patches", "provider-patch"),
        ] {
            let Some(Value::Array(entries)) = refs.get(group) else {
                continue;
            };
            for entry in entries {
                if let Some(reference) = run_result_ref_uri(entry) {
                    push_unique_evidence_ref(&mut outcome.evidence_refs, kind, reference, group);
                    surfaced_evidence = true;
                }
            }
        }
    }

    // 4. Empty shell guard: a failed run-result that surfaced no diagnostics,
    //    no provider error, no run/runtime identity, and no refs must still
    //    explain itself rather than appear as an opaque empty failure.
    if !surfaced_evidence {
        push_unique_diagnostic(
            &mut outcome.diagnostics,
            "provider.run_result_empty".to_string(),
            "Provider run-result reported failure but produced no diagnostics, \
             provider error, run/runtime id, or log/transcript refs: no provider \
             runtime or session appears to have been created."
                .to_string(),
            json!({ "run_status": run_status }),
        );
    }
}

/// Treat `null`, empty string, empty object, and empty array as "no value" so an
/// empty `provider_error: {}` shell is not mistaken for real evidence.
fn is_empty_value(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::String(text) => text.trim().is_empty(),
        Value::Object(map) => map.is_empty(),
        Value::Array(items) => items.is_empty(),
        _ => false,
    }
}

/// Build a human-readable failure message from the run-result identity fields.
fn describe_run_result_failure(
    run_status: Option<&str>,
    run_id: Option<&str>,
    runtime_id: Option<&str>,
    runtime_status: Option<&str>,
    provider_error: Option<&Value>,
) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(error) = provider_error {
        if let Some(message) = provider_error_message(error) {
            parts.push(format!("provider error: {message}"));
        } else {
            parts.push("provider error reported".to_string());
        }
    }
    if let Some(run_id) = run_id {
        parts.push(format!("run_id={run_id}"));
    }
    if let Some(runtime_id) = runtime_id {
        parts.push(format!("runtime_id={runtime_id}"));
    }
    if let Some(runtime_status) = runtime_status {
        parts.push(format!("runtime_status={runtime_status}"));
    }
    if parts.is_empty() {
        return format!(
            "Provider run-result failed (status={}).",
            run_status.unwrap_or("failed")
        );
    }
    format!("Provider run-result failed: {}.", parts.join(", "))
}

/// Extract a short error message from a provider_error value, accepting either a
/// string or an object carrying `message`/`error`/`detail`/`code`.
fn provider_error_message(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => {
            let trimmed = text.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_string())
        }
        Value::Object(map) => map
            .get("message")
            .or_else(|| map.get("error"))
            .or_else(|| map.get("detail"))
            .or_else(|| map.get("code"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string),
        _ => None,
    }
}

/// Mirror the provider_error onto `outcome.metadata.provider_error` without
/// clobbering an existing populated value.
fn mirror_provider_error_metadata(outcome: &mut AgentTaskOutcome, provider_error: Value) {
    let mut metadata = outcome.metadata.as_object().cloned().unwrap_or_default();
    let already_populated = metadata
        .get("provider_error")
        .is_some_and(|existing| !is_empty_value(existing));
    if !already_populated {
        metadata.insert("provider_error".to_string(), provider_error);
        outcome.metadata = Value::Object(metadata);
    }
}

/// Resolve a usable reference URI/path from a run-result ref entry, accepting a
/// bare string or an object carrying `uri`/`url`/`path`/`ref`/`id`.
fn run_result_ref_uri(entry: &Value) -> Option<String> {
    match entry {
        Value::String(text) => {
            let trimmed = text.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_string())
        }
        Value::Object(map) => map
            .get("uri")
            .or_else(|| map.get("url"))
            .or_else(|| map.get("path"))
            .or_else(|| map.get("ref"))
            .or_else(|| map.get("id"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string),
        _ => None,
    }
}

/// Push a diagnostic unless an identical (class, message) is already present.
fn push_unique_diagnostic(
    diagnostics: &mut Vec<AgentTaskDiagnostic>,
    class: String,
    message: String,
    data: Value,
) {
    if diagnostics
        .iter()
        .any(|existing| existing.class == class && existing.message == message)
    {
        return;
    }
    diagnostics.push(AgentTaskDiagnostic {
        class,
        message,
        data,
    });
}

/// Push an evidence ref unless an identical (kind, uri) is already present.
fn push_unique_evidence_ref(
    evidence_refs: &mut Vec<AgentTaskEvidenceRef>,
    kind: &str,
    uri: String,
    group: &str,
) {
    if evidence_refs
        .iter()
        .any(|existing| existing.kind == kind && existing.uri == uri)
    {
        return;
    }
    evidence_refs.push(AgentTaskEvidenceRef {
        kind: kind.to_string(),
        uri,
        label: Some(format!("provider run-result {group}")),
    });
}

fn normalize_provider_runtime_contract(
    outcome: &mut AgentTaskOutcome,
    provider: &AgentTaskExecutorProvider,
) {
    let normalization = &provider.runtime_contract.normalization;
    if let Some(summary_path) = normalization.summary_path.as_deref() {
        if let Some(summary) = dotted_value(outcome, summary_path).and_then(Value::as_str) {
            if !summary.trim().is_empty() {
                outcome.summary = Some(summary.to_string());
            }
        }
    }

    if let Some(status_path) = normalization.status_path.as_deref() {
        if let Some(status) = dotted_value(outcome, status_path).and_then(Value::as_str) {
            if let Some(mapped_status) = provider
                .runtime_contract
                .lifecycle_states
                .outcome_statuses
                .get(status)
                .copied()
            {
                outcome.status = mapped_status;
            }
            if let Some(mapped_classification) = provider
                .runtime_contract
                .lifecycle_states
                .failure_classifications
                .get(status)
                .copied()
            {
                outcome.failure_classification = Some(mapped_classification);
            } else if outcome.status != AgentTaskOutcomeStatus::Succeeded
                && outcome.failure_classification.is_none()
            {
                outcome.failure_classification = Some(AgentTaskFailureClassification::Unknown);
            }
        }
    }

    for mapping in &normalization.output_artifacts {
        let Some(value) = dotted_value(outcome, &mapping.path).cloned() else {
            continue;
        };
        normalize_provider_runtime_artifact(outcome, mapping, value);
    }
}

fn normalize_provider_runtime_artifact(
    outcome: &mut AgentTaskOutcome,
    mapping: &AgentTaskRuntimeOutputArtifactMapping,
    value: Value,
) {
    let id = mapping.id.clone().unwrap_or_else(|| mapping.name.clone());
    if outcome.artifacts.iter().any(|artifact| artifact.id == id) {
        return;
    }

    let path = value.as_str().map(str::to_string).or_else(|| {
        value
            .get("path")
            .and_then(Value::as_str)
            .map(str::to_string)
    });
    let url = value.get("url").and_then(Value::as_str).map(str::to_string);
    let artifact = AgentTaskArtifact {
        schema: AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
        id: id.clone(),
        kind: mapping
            .kind
            .clone()
            .or_else(|| mapping.artifact_type.clone())
            .unwrap_or_else(|| mapping.name.clone()),
        name: Some(mapping.name.clone()),
        label: None,
        role: None,
        semantic_key: None,
        path,
        url,
        mime: mapping.mime.clone(),
        size_bytes: None,
        sha256: None,
        metadata: json!({
            "runtime_contract": true,
            "source_path": mapping.path,
        }),
    };

    outcome.artifacts.push(artifact.clone());
    if mapping.artifact_type.is_some() || mapping.artifact_schema.is_some() {
        outcome.typed_artifacts.push(AgentTaskTypedArtifact {
            name: mapping.name.clone(),
            artifact_type: mapping.artifact_type.clone(),
            artifact_schema: mapping.artifact_schema.clone(),
            payload: value,
            artifact: Some(artifact),
            metadata: json!({ "runtime_contract": true }),
        });
    }
}

fn dotted_value<'a>(outcome: &'a AgentTaskOutcome, path: &str) -> Option<&'a Value> {
    let mut parts = path.split('.');
    let first = parts.next()?.trim();
    let mut current = match first {
        "outputs" => &outcome.outputs,
        "metadata" => &outcome.metadata,
        _ => return None,
    };
    for part in parts {
        current = current.get(part.trim())?;
    }
    Some(current).filter(|value| !value.is_null())
}

fn normalize_provider_artifact_roles(
    artifacts: &mut [AgentTaskArtifact],
    role_aliases: &AgentTaskProviderRoleAliases,
) {
    for artifact in artifacts {
        let Some(role) = role_aliases.role_for_artifact_kind(&artifact.kind) else {
            continue;
        };
        let original_kind = artifact.kind.clone();
        artifact.kind = role.to_string();
        if !artifact.metadata.is_object() {
            artifact.metadata = json!({});
        }
        if let Some(metadata) = artifact.metadata.as_object_mut() {
            metadata.entry("role".to_string()).or_insert(json!(role));
            metadata
                .entry("provider_kind".to_string())
                .or_insert(json!(original_kind));
        }
    }
}

fn normalize_provider_run_result_output(
    outcome: &mut AgentTaskOutcome,
    role_aliases: &AgentTaskProviderRoleAliases,
) {
    if output_value(&outcome.outputs, "provider_run_result").is_some() {
        return;
    }

    let value = role_aliases
        .output_aliases_for_role("provider_run_result")
        .into_iter()
        .find_map(|alias| output_value(&outcome.outputs, alias))
        .or_else(|| {
            role_aliases
                .metadata_aliases_for_role("provider_run_result")
                .into_iter()
                .find_map(|alias| output_value(&outcome.metadata, alias))
        });

    if let Some(value) = value.cloned() {
        let mut outputs = outcome.outputs.as_object().cloned().unwrap_or_default();
        outputs.insert("provider_run_result".to_string(), value);
        outcome.outputs = Value::Object(outputs);
    }
}

fn output_value<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
    value.get(key).filter(|value| !value.is_null())
}

fn render_provider_command_display(provider: &AgentTaskExecutorProvider) -> String {
    if let Some(display) = provider.invocation.display.as_deref() {
        return render_provider_command_template(display, provider);
    }
    if !provider.invocation.argv.is_empty() {
        return render_provider_invocation_argv(provider).join(" ");
    }
    if !provider.command_argv.is_empty() {
        return render_provider_command_argv(provider).join(" ");
    }

    render_provider_command_string(provider)
}

fn render_provider_command_string(provider: &AgentTaskExecutorProvider) -> String {
    render_provider_command_template(&provider.command, provider)
}

fn render_provider_command_template(value: &str, provider: &AgentTaskExecutorProvider) -> String {
    let extension_path = provider.extension_path.as_deref().unwrap_or_default();
    let runtime_path = provider.runtime_path.as_deref().unwrap_or(extension_path);
    value
        .replace("{{extension_path}}", extension_path)
        .replace("{{runtime_path}}", runtime_path)
}

fn render_provider_command_argv(provider: &AgentTaskExecutorProvider) -> Vec<String> {
    provider
        .command_argv
        .iter()
        .map(|arg| render_provider_command_template(arg, provider))
        .collect()
}

fn render_provider_invocation_argv(provider: &AgentTaskExecutorProvider) -> Vec<String> {
    provider
        .invocation
        .argv
        .iter()
        .map(|arg| render_provider_command_template(arg, provider))
        .collect()
}

fn provider_command_parts(
    provider: &AgentTaskExecutorProvider,
) -> Option<(String, Vec<String>, Option<PathBuf>)> {
    let (argv, cwd) = if !provider.invocation.argv.is_empty() {
        (
            render_provider_invocation_argv(provider),
            provider
                .invocation
                .cwd
                .as_deref()
                .map(|cwd| PathBuf::from(render_provider_command_template(cwd, provider))),
        )
    } else if provider.command_argv.is_empty() {
        // Legacy string commands retain their historical split behavior for
        // compatibility. New provider manifests should use command_argv/argv.
        eprintln!(
            "Warning: agent task provider '{}' uses deprecated string command; use invocation.argv or argv instead",
            provider.id
        );
        (
            render_provider_command_string(provider)
                .split_whitespace()
                .map(str::to_string)
                .collect(),
            None,
        )
    } else {
        (render_provider_command_argv(provider), None)
    };
    let mut parts = argv.into_iter();
    let program = parts.next()?;
    Some((program, parts.collect(), cwd))
}

fn provider_command_env(
    request: &AgentTaskRequest,
    provider: &AgentTaskExecutorProvider,
) -> Result<Vec<(String, String)>, ProviderCommandEnvError> {
    // Both runtime path env vars resolve to the provider runtime_path, falling
    // back to the extension_path when the runtime is not separately declared.
    let runtime_path = provider
        .runtime_path
        .clone()
        .or_else(|| provider.extension_path.clone())
        .unwrap_or_default();
    let mut env = vec![
        (
            "HOMEBOY_AGENT_TASK_PROVIDER_ID".to_string(),
            provider.id.clone(),
        ),
        (
            "HOMEBOY_AGENT_TASK_EXECUTOR_CONFIG_JSON".to_string(),
            serde_json::to_string(&request.executor.config).unwrap_or_else(|_| "null".to_string()),
        ),
        (
            "HOMEBOY_AGENT_TASK_SECRET_ENV_PLAN_JSON".to_string(),
            serde_json::to_string(&provider_secret_env_plan_with_status(provider, request))
                .unwrap_or_else(|_| "null".to_string()),
        ),
        (
            "HOMEBOY_AGENT_TOOL_POLICY_JSON".to_string(),
            serde_json::to_string(&request.policy.tools).unwrap_or_else(|_| "null".to_string()),
        ),
        (
            "HOMEBOY_AGENT_TOOL_REQUEST_SCHEMA".to_string(),
            AGENT_TOOL_REQUEST_SCHEMA.to_string(),
        ),
        (
            "HOMEBOY_AGENT_TOOL_RESULT_SCHEMA".to_string(),
            AGENT_TOOL_RESULT_SCHEMA.to_string(),
        ),
        (
            "HOMEBOY_AGENT_TOOL_POLICY_SCHEMA".to_string(),
            AGENT_TOOL_POLICY_SCHEMA.to_string(),
        ),
        (
            "HOMEBOY_AGENT_TOOL_DISPATCH_COMMAND".to_string(),
            agent_tool_dispatch_command(),
        ),
        (
            "HOMEBOY_EXTENSION_ID".to_string(),
            provider.extension_id.clone().unwrap_or_default(),
        ),
        (
            "HOMEBOY_EXTENSION_PATH".to_string(),
            provider.extension_path.clone().unwrap_or_default(),
        ),
        ("HOMEBOY_RUNTIME_PATH".to_string(), runtime_path.clone()),
        (
            "HOMEBOY_AGENT_RUNTIME_ID".to_string(),
            provider.runtime_id.clone().unwrap_or_default(),
        ),
        ("HOMEBOY_AGENT_RUNTIME_PATH".to_string(), runtime_path),
    ];
    env.extend(provider_executable_env(provider).map_err(ProviderCommandEnvError::Executable)?);
    env.extend(
        resolve_secret_env_with_fallbacks(
            &request.executor.secret_env,
            &provider_secret_sources(provider, Some(request)),
        )
        .map_err(ProviderCommandEnvError::Secret)?,
    );
    Ok(env)
}

fn agent_tool_dispatch_command() -> String {
    let current_exe = std::env::current_exe()
        .map(|path| path.to_string_lossy().to_string())
        .expect("current executable path is required for agent tool dispatch command");
    format!(
        "{} agent-task tool dispatch",
        shell::quote_arg(&current_exe)
    )
}

fn failure_outcome(
    request: &AgentTaskRequest,
    status: AgentTaskOutcomeStatus,
    classification: AgentTaskFailureClassification,
    diagnostic_class: &str,
    message: String,
    data: Value,
) -> AgentTaskOutcome {
    AgentTaskOutcome {
        schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
        task_id: request.task_id.clone(),
        status,
        summary: Some(message.clone()),
        failure_classification: Some(classification),
        artifacts: Vec::new(),
        typed_artifacts: Vec::new(),
        evidence_refs: vec![AgentTaskEvidenceRef {
            kind: "agent-task-provider".to_string(),
            uri: format!("homeboy://agent-task/{}", diagnostic_class),
            label: Some("agent task provider dispatch".to_string()),
        }],
        diagnostics: vec![AgentTaskDiagnostic {
            class: diagnostic_class.to_string(),
            message,
            data,
        }],
        outputs: Value::Null,
        workflow: None,
        follow_up: None,
        metadata: Value::Null,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::agent_task::{
        AgentTaskExecutor, AgentTaskLimits, AgentTaskPolicy, AgentTaskWorkspace,
        AgentTaskWorkspaceMode, AgentToolExecutionLocation, AgentToolPolicyRule,
    };
    use crate::core::agent_task_scheduler::{
        AgentTaskCancellationToken, AgentTaskExecutionContext, AgentTaskPlan, AgentTaskScheduler,
    };
    use std::fs;

    #[test]
    fn provider_capability_contract_exports_core_owned_schema_ids() {
        let contract = provider_capability_contract();

        assert_eq!(
            contract.schema,
            AGENT_TASK_PROVIDER_CAPABILITY_CONTRACT_SCHEMA
        );
        assert_eq!(
            contract.provider_schema,
            AGENT_TASK_EXECUTOR_PROVIDER_SCHEMA
        );
        assert_eq!(contract.request_schema, AGENT_TASK_REQUEST_SCHEMA);
        assert_eq!(contract.outcome_schema, AGENT_TASK_OUTCOME_SCHEMA);
        assert_eq!(contract.tool_request_schema, AGENT_TOOL_REQUEST_SCHEMA);
        assert_eq!(contract.tool_result_schema, AGENT_TOOL_RESULT_SCHEMA);
        assert_eq!(contract.tool_policy_schema, AGENT_TOOL_POLICY_SCHEMA);
    }

    #[test]
    fn provider_manifest_defaults_core_owned_schema_ids() {
        let provider: AgentTaskExecutorProvider = serde_json::from_value(json!({
            "id": "minimal.provider",
            "backend": "minimal",
            "command": "minimal-provider"
        }))
        .expect("provider manifest");

        assert_eq!(provider.schema, AGENT_TASK_EXECUTOR_PROVIDER_SCHEMA);
        assert_eq!(provider.request_schema, AGENT_TASK_REQUEST_SCHEMA);
        assert_eq!(provider.outcome_schema, AGENT_TASK_OUTCOME_SCHEMA);
    }

    #[test]
    fn provider_manifest_accepts_typed_command_argv_aliases() {
        let provider: AgentTaskExecutorProvider = serde_json::from_value(json!({
            "id": "argv.provider",
            "backend": "argv",
            "command": "legacy-provider --legacy",
            "argv": ["{{extension_path}}/bin/provider", "--runtime", "{{runtime_path}}"]
        }))
        .expect("provider manifest");

        assert_eq!(provider.command, "legacy-provider --legacy");
        assert_eq!(
            provider.command_argv,
            vec![
                "{{extension_path}}/bin/provider".to_string(),
                "--runtime".to_string(),
                "{{runtime_path}}".to_string(),
            ]
        );
    }

    #[test]
    fn provider_manifest_accepts_command_invocation_contract() {
        let provider: AgentTaskExecutorProvider = serde_json::from_value(json!({
            "id": "invocation.provider",
            "backend": "invocation",
            "command": "legacy-provider --legacy",
            "invocation": {
                "schema": "homeboy/command-invocation/v1",
                "argv": ["{{runtime_path}}/bin/provider", "--json"],
                "cwd": "{{runtime_path}}",
                "env": [{ "name": "TOKEN", "source": "secret_env", "redacted": true }],
                "display": "provider --json",
                "redaction": { "env": ["TOKEN"] }
            }
        }))
        .expect("provider manifest");

        assert_eq!(provider.invocation.argv[0], "{{runtime_path}}/bin/provider");
        assert_eq!(provider.invocation.cwd.as_deref(), Some("{{runtime_path}}"));
        assert_eq!(provider.invocation.env[0].name, "TOKEN");
        assert_eq!(
            provider.invocation.display.as_deref(),
            Some("provider --json")
        );
        assert_eq!(provider.invocation.redaction.env, vec!["TOKEN"]);
    }

    #[test]
    fn provider_command_parts_warns_for_legacy_string_command() {
        let (_request, provider) =
            request("task-legacy-command", "legacy-provider --flag".to_string());

        let (program, args, cwd) = provider_command_parts(&provider).expect("command parts");

        assert_eq!(program, "legacy-provider");
        assert_eq!(args, vec!["--flag"]);
        assert_eq!(cwd, None);
    }

    #[test]
    fn provider_manifest_preserves_unknown_metadata_on_export() {
        let provider: AgentTaskExecutorProvider = serde_json::from_value(json!({
            "id": "metadata.provider",
            "backend": "metadata",
            "command": "metadata-provider",
            "provider_metadata": {
                "runtime": "provider-owned"
            },
            "runner_readiness": [{
                "id": "ready",
                "label": "Ready",
                "provider_hint": "preserve-me",
                "env_path": {
                    "env": ["PROVIDER_HOME"],
                    "path_kind": "provider-cache"
                },
                "executable": {
                    "env": ["PROVIDER_BIN"],
                    "candidates": ["provider-bin"],
                    "version_command": ["--version"],
                    "install_hint": "Install provider-bin.",
                    "provider_executable_hint": "preserve-me"
                }
            }],
            "timeout_artifact_discovery": {
                "paths": ["artifacts"],
                "provider_discovery": true,
                "artifact_patterns": [{
                    "kind": "log",
                    "filename_contains": ["provider"],
                    "provider_role": "diagnostic"
                }]
            },
            "role_aliases": {
                "outputs": { "patch": ["diff"] },
                "provider_alias_policy": "strict"
            },
            "runtime_contract": {
                "capabilities": ["sandbox", "artifacts"],
                "lifecycle_states": {
                    "execution_states": { "queued": "queued", "done": "succeeded" },
                    "outcome_statuses": { "ok": "succeeded", "error": "failed" },
                    "failure_classifications": { "error": "provider" }
                },
                "normalization": {
                    "status_path": "outputs.runtime.status",
                    "summary_path": "outputs.runtime.summary",
                    "output_artifacts": [{
                        "name": "patch",
                        "type": "patch",
                        "artifact_schema": "text/x-patch",
                        "path": "outputs.runtime.artifacts.patch",
                        "kind": "patch"
                    }]
                }
            },
            "workspace_materialization": {
                "cwd": "git_checkout",
                "provider_workspace_mode": "linked"
            }
        }))
        .expect("provider manifest");

        assert_eq!(
            provider.extra["provider_metadata"]["runtime"],
            "provider-owned"
        );
        assert_eq!(
            provider.runner_readiness[0].extra["provider_hint"],
            "preserve-me"
        );
        assert_eq!(
            provider.runner_readiness[0]
                .env_path
                .as_ref()
                .expect("env path")
                .extra["path_kind"],
            "provider-cache"
        );
        let executable = provider.runner_readiness[0]
            .executable
            .as_ref()
            .expect("executable readiness");
        assert_eq!(executable.env, vec!["PROVIDER_BIN".to_string()]);
        assert_eq!(executable.candidates, vec!["provider-bin".to_string()]);
        assert_eq!(executable.version_command, vec!["--version".to_string()]);
        assert_eq!(
            executable.install_hint.as_deref(),
            Some("Install provider-bin.")
        );
        assert_eq!(executable.extra["provider_executable_hint"], "preserve-me");
        assert_eq!(
            provider.timeout_artifact_discovery.extra["provider_discovery"],
            true
        );
        assert_eq!(
            provider.timeout_artifact_discovery.artifact_patterns[0].extra["provider_role"],
            "diagnostic"
        );
        assert_eq!(
            provider.role_aliases.extra["provider_alias_policy"],
            "strict"
        );
        assert_eq!(
            provider.runtime_contract.capabilities,
            vec!["sandbox".to_string(), "artifacts".to_string()]
        );
        assert_eq!(
            provider
                .runtime_contract
                .lifecycle_states
                .outcome_statuses
                .get("ok"),
            Some(&AgentTaskOutcomeStatus::Succeeded)
        );
        assert_eq!(
            provider.runtime_contract.normalization.output_artifacts[0].name,
            "patch"
        );
        assert_eq!(
            provider
                .workspace_materialization
                .as_ref()
                .expect("workspace materialization")
                .extra["provider_workspace_mode"],
            "linked"
        );

        let exported = serde_json::to_value(&provider).expect("provider export");
        assert_eq!(exported["provider_metadata"]["runtime"], "provider-owned");
        assert_eq!(
            exported["runner_readiness"][0]["provider_hint"],
            "preserve-me"
        );
        assert_eq!(
            exported["runner_readiness"][0]["env_path"]["path_kind"],
            "provider-cache"
        );
        assert_eq!(
            exported["runner_readiness"][0]["executable"]["version_command"][0],
            "--version"
        );
        assert_eq!(
            exported["timeout_artifact_discovery"]["provider_discovery"],
            true
        );
        assert_eq!(
            exported["timeout_artifact_discovery"]["artifact_patterns"][0]["provider_role"],
            "diagnostic"
        );
        assert_eq!(exported["role_aliases"]["provider_alias_policy"], "strict");
        assert_eq!(exported["runtime_contract"]["capabilities"][0], "sandbox");
        assert_eq!(
            exported["runtime_contract"]["normalization"]["output_artifacts"][0]["name"],
            "patch"
        );
        assert_eq!(
            exported["workspace_materialization"]["provider_workspace_mode"],
            "linked"
        );
    }

    #[test]
    fn runtime_contract_normalizes_provider_outputs_to_canonical_artifacts() {
        let provider_output = json!({
            "schema": AGENT_TASK_OUTCOME_SCHEMA,
            "task_id": "task-runtime-normalization",
            "status": "succeeded",
            "outputs": {
                "runtime": {
                    "status": "done",
                    "summary": "runtime finished",
                    "artifacts": {
                        "patch": "/tmp/runtime.patch",
                        "report": { "path": "/tmp/report.json" }
                    }
                }
            }
        });
        let script = script(&format!(
            "process.stdout.write(JSON.stringify({}));",
            provider_output
        ));
        let (request, mut provider) =
            request("task-runtime-normalization", format!("node {script}"));
        provider.runtime_contract = AgentTaskRuntimeContract {
            capabilities: vec!["sandbox".to_string()],
            lifecycle_states: AgentTaskRuntimeLifecycleStates {
                execution_states: BTreeMap::new(),
                outcome_statuses: BTreeMap::from([(
                    "done".to_string(),
                    AgentTaskOutcomeStatus::Succeeded,
                )]),
                failure_classifications: BTreeMap::new(),
            },
            normalization: AgentTaskRuntimeNormalization {
                status_path: Some("outputs.runtime.status".to_string()),
                summary_path: Some("outputs.runtime.summary".to_string()),
                output_artifacts: vec![
                    AgentTaskRuntimeOutputArtifactMapping {
                        name: "patch".to_string(),
                        artifact_type: Some("patch".to_string()),
                        artifact_schema: Some("text/x-patch".to_string()),
                        path: "outputs.runtime.artifacts.patch".to_string(),
                        kind: Some("patch".to_string()),
                        mime: Some("text/x-patch".to_string()),
                        id: None,
                    },
                    AgentTaskRuntimeOutputArtifactMapping {
                        name: "report".to_string(),
                        artifact_type: Some("agent_report".to_string()),
                        artifact_schema: Some("application/json".to_string()),
                        path: "outputs.runtime.artifacts.report".to_string(),
                        kind: Some("report".to_string()),
                        mime: Some("application/json".to_string()),
                        id: None,
                    },
                ],
            },
            apply_back: AgentTaskRuntimeApplyBack::default(),
        };

        let outcome = run_provider_command(&request, &provider);

        assert_eq!(outcome.status, AgentTaskOutcomeStatus::Succeeded);
        assert_eq!(outcome.summary.as_deref(), Some("runtime finished"));
        assert_eq!(outcome.artifacts.len(), 2);
        assert_eq!(outcome.artifacts[0].kind, "patch");
        assert_eq!(
            outcome.artifacts[0].path.as_deref(),
            Some("/tmp/runtime.patch")
        );
        assert_eq!(outcome.artifacts[1].kind, "report");
        assert_eq!(
            outcome.artifacts[1].path.as_deref(),
            Some("/tmp/report.json")
        );
        assert_eq!(outcome.typed_artifacts.len(), 2);
        assert_eq!(outcome.typed_artifacts[0].name, "patch");
        assert_eq!(
            outcome.typed_artifacts[1].artifact_schema.as_deref(),
            Some("application/json")
        );
    }

    #[test]
    fn runtime_contract_maps_failed_runtime_status() {
        let provider_output = json!({
            "schema": AGENT_TASK_OUTCOME_SCHEMA,
            "task_id": "task-runtime-failed",
            "status": "succeeded",
            "outputs": { "sample_runtime": { "state": "failed" } }
        });
        let script = script(&format!(
            "process.stdout.write(JSON.stringify({}));",
            provider_output
        ));
        let (request, mut provider) = request("task-runtime-failed", format!("node {script}"));
        provider.backend = "sample-runtime".to_string();
        provider.runtime_contract.lifecycle_states.outcome_statuses =
            BTreeMap::from([("failed".to_string(), AgentTaskOutcomeStatus::Failed)]);
        provider
            .runtime_contract
            .lifecycle_states
            .failure_classifications = BTreeMap::from([(
            "failed".to_string(),
            AgentTaskFailureClassification::Provider,
        )]);
        provider.runtime_contract.normalization.status_path =
            Some("outputs.sample_runtime.state".to_string());

        let outcome = run_provider_command(&request, &provider);

        assert_eq!(outcome.status, AgentTaskOutcomeStatus::Failed);
        assert_eq!(
            outcome.failure_classification,
            Some(AgentTaskFailureClassification::Provider)
        );
    }

    #[test]
    fn readiness_validation_fails_before_execution_when_provider_executable_is_missing() {
        let (_request, mut provider) = request("task-readiness", "minimal-provider".to_string());
        provider.runner_readiness = vec![AgentTaskProviderRunnerReadiness {
            id: "test.executable".to_string(),
            label: "Test executable".to_string(),
            secret_env: Vec::new(),
            env_path: None,
            executable: Some(AgentTaskProviderExecutableReadiness {
                env: vec!["HOMEBOY_TEST_PROVIDER_COMMAND".to_string()],
                candidates: vec![format!(
                    "homeboy-definitely-missing-provider-{}",
                    std::process::id()
                )],
                version_command: Vec::new(),
                install_hint: Some("Install the test provider".to_string()),
                extra: BTreeMap::new(),
            }),
            remediation: None,
            extra: BTreeMap::new(),
        }];

        let err = validate_provider_runner_readiness_for_backend_with_providers(
            &[provider],
            "test",
            None,
        )
        .expect_err("missing provider executable should block preflight");

        assert_eq!(err.code.as_str(), "validation.invalid_argument");
        assert!(err.message.contains("backend 'test' is registered"));
        assert!(err
            .message
            .contains("provider runner executable 'Test executable'"));
        assert!(err.message.contains("HOMEBOY_TEST_PROVIDER_COMMAND"));
        assert!(err.message.contains("Install the test provider"));
    }

    #[test]
    fn provider_command_env_exposes_generic_agent_tool_contracts() {
        let (mut request, provider) = request("task-1", "minimal-provider".to_string());
        request.policy.tools.tools.insert(
            "lookup".to_string(),
            AgentToolPolicyRule {
                execution_location: AgentToolExecutionLocation::ControlPlane,
                timeout_ms: Some(500),
                reason: Some("test policy".to_string()),
            },
        );

        let env = provider_command_env(&request, &provider).expect("provider env");
        let env: BTreeMap<String, String> = env.into_iter().collect();

        assert_eq!(
            env.get("HOMEBOY_AGENT_TOOL_REQUEST_SCHEMA")
                .map(String::as_str),
            Some(AGENT_TOOL_REQUEST_SCHEMA)
        );
        assert_eq!(
            env.get("HOMEBOY_AGENT_TOOL_RESULT_SCHEMA")
                .map(String::as_str),
            Some(AGENT_TOOL_RESULT_SCHEMA)
        );
        assert_eq!(
            env.get("HOMEBOY_AGENT_TOOL_POLICY_SCHEMA")
                .map(String::as_str),
            Some(AGENT_TOOL_POLICY_SCHEMA)
        );
        let dispatch_command = env
            .get("HOMEBOY_AGENT_TOOL_DISPATCH_COMMAND")
            .expect("tool dispatch command env");
        assert!(
            dispatch_command.ends_with(" agent-task tool dispatch"),
            "dispatch command should invoke hidden tool dispatch command: {dispatch_command}"
        );
        assert!(
            dispatch_command.starts_with('/') || dispatch_command.starts_with('\''),
            "dispatch command should start with an absolute executable path, shell quoted when needed: {dispatch_command}"
        );

        let policy: crate::core::agent_task::AgentToolPolicy = serde_json::from_str(
            env.get("HOMEBOY_AGENT_TOOL_POLICY_JSON")
                .expect("tool policy env"),
        )
        .expect("tool policy json");
        assert_eq!(
            policy.execution_location_for("lookup"),
            AgentToolExecutionLocation::ControlPlane
        );
        assert_eq!(
            policy.execution_location_for("unknown"),
            AgentToolExecutionLocation::Disabled
        );
    }

    fn script(body: &str) -> String {
        let path = std::env::temp_dir().join(format!(
            "homeboy-agent-task-provider-{}-{}.js",
            std::process::id(),
            body.len()
        ));
        fs::write(&path, body).expect("script written");
        path.to_string_lossy().to_string()
    }

    fn request(task_id: &str, command: String) -> (AgentTaskRequest, AgentTaskExecutorProvider) {
        let provider = AgentTaskExecutorProvider {
            schema: AGENT_TASK_EXECUTOR_PROVIDER_SCHEMA.to_string(),
            id: "test.provider".to_string(),
            label: None,
            backend: "test".to_string(),
            default_backend: false,
            command,
            command_argv: Vec::new(),
            invocation: CommandInvocation::default(),
            request_schema: AGENT_TASK_REQUEST_SCHEMA.to_string(),
            outcome_schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
            capabilities: vec!["structured_outcome".to_string()],
            secret_requirements: Vec::new(),
            secret_env_requirements: Vec::new(),
            workspace_materialization: None,
            provider_defaults: BTreeMap::new(),
            runner_readiness: Vec::new(),
            runner_sources: Vec::new(),
            dependency_failure_patterns: Vec::new(),
            timeout_artifact_discovery: AgentTaskProviderTimeoutArtifactDiscovery::default(),
            role_aliases: AgentTaskProviderRoleAliases::default(),
            runtime_contract: AgentTaskRuntimeContract::default(),
            extension_id: None,
            extension_path: None,
            runtime_id: None,
            runtime_path: None,
            extra: BTreeMap::new(),
        };
        let request = AgentTaskRequest {
            schema: AGENT_TASK_REQUEST_SCHEMA.to_string(),
            task_id: task_id.to_string(),
            group_key: None,
            parent_plan_id: None,
            executor: AgentTaskExecutor {
                backend: "test".to_string(),
                selector: None,
                runtime_selection: None,
                required_capabilities: Vec::new(),
                secret_env: Vec::new(),
                model: None,
                config: Value::Null,
            },
            instructions: "run".to_string(),
            inputs: Value::Null,
            source_refs: Vec::new(),
            workspace: AgentTaskWorkspace::default(),
            component_contracts: Vec::new(),
            policy: AgentTaskPolicy::default(),
            limits: AgentTaskLimits::default(),
            expected_artifacts: Vec::new(),
            artifact_declarations: Vec::new(),
            metadata: Value::Null,
        };
        (request, provider)
    }

    #[test]
    fn provider_preflight_reports_missing_command_before_spawn() {
        let (request, provider) = request(
            "missing-command",
            "homeboy-definitely-missing-provider-command --json".to_string(),
        );

        let outcome = run_provider_command_once(&request, &provider);

        assert_eq!(outcome.status, AgentTaskOutcomeStatus::ProviderError);
        assert_eq!(
            outcome.diagnostics[0].class,
            "agent_task.provider_command_unavailable"
        );
        assert_eq!(
            outcome.diagnostics[0].data["program"],
            "homeboy-definitely-missing-provider-command"
        );
        assert!(outcome.diagnostics[0].data["failures"][0]["remediation"]
            .as_str()
            .expect("remediation")
            .contains("PATH"));
    }

    #[test]
    fn provider_preflight_reports_missing_secret_readiness() {
        let (request, mut provider) = request(
            "missing-secret",
            std::env::current_exe()
                .expect("current exe")
                .display()
                .to_string(),
        );
        provider.secret_requirements = vec![AgentTaskProviderSecretRequirement {
            name: None,
            env: vec!["HOMEBOY_TEST_PROVIDER_SECRET_THAT_SHOULD_NOT_EXIST".to_string()],
            required: Some(true),
            purpose: Some("test".to_string()),
            extra: BTreeMap::new(),
        }];

        let outcome = run_provider_command_once(&request, &provider);

        assert_eq!(outcome.status, AgentTaskOutcomeStatus::ProviderError);
        assert_eq!(
            outcome.diagnostics[0].class,
            "agent_task.secret_env_missing"
        );
        assert_eq!(
            outcome.diagnostics[0].data["secret_env_status"][0]["name"],
            "HOMEBOY_TEST_PROVIDER_SECRET_THAT_SHOULD_NOT_EXIST"
        );
        assert_eq!(
            outcome.diagnostics[0].data["secret_env_status"][0]["configured"],
            false
        );
    }

    #[test]
    fn required_extension_ids_follow_selected_agent_task_providers() {
        let (request_a, mut provider_a) = request("task-a", "node provider-a.js".to_string());
        provider_a.id = "provider-a".to_string();
        provider_a.extension_id = Some("extension-a".to_string());
        let (mut request_b, mut provider_b) = request("task-b", "node provider-b.js".to_string());
        request_b.executor.selector = Some("provider-b".to_string());
        provider_b.id = "provider-b".to_string();
        provider_b.extension_id = Some("extension-b".to_string());
        let executor =
            ExtensionProviderAgentTaskExecutor::with_providers(vec![provider_a, provider_b]);

        let extension_ids = executor.required_extension_ids_for_plan(&AgentTaskPlan::new(
            "plan-a",
            vec![request_a, request_b],
        ));

        assert_eq!(extension_ids, vec!["extension-a", "extension-b"]);
    }

    #[test]
    fn repo_local_gate_execution_kind_runs_without_extension_provider() {
        let temp = tempfile::tempdir().expect("tempdir");
        fs::write(
            temp.path().join("gate.mjs"),
            "import fs from 'node:fs'; fs.writeFileSync(process.env.RESULT_PATH, JSON.stringify({ok:true}));",
        )
        .expect("write script");
        let (mut request, _) = request("repo-local-gate", "unused".to_string());
        request.executor.backend = "agent-task".to_string();
        request.executor.config = json!({
            "execution_kind": "repo_local_gate",
            "script": "gate.mjs",
            "artifact_outputs": {
                "result": { "schema": "example/Result/v1" }
            }
        });
        request.workspace = AgentTaskWorkspace {
            mode: AgentTaskWorkspaceMode::Existing,
            root: Some(temp.path().display().to_string()),
            ..AgentTaskWorkspace::default()
        };
        let executor = ExtensionProviderAgentTaskExecutor::with_providers(Vec::new());

        let outcome = executor.execute(
            request,
            AgentTaskExecutionContext {
                plan_id: "gate-plan".to_string(),
                attempt: 1,
                cancellation: AgentTaskCancellationToken::default(),
            },
        );

        assert_eq!(outcome.status, AgentTaskOutcomeStatus::Succeeded);
        assert_eq!(outcome.outputs["result"]["ok"], true);
        assert_eq!(outcome.typed_artifacts.len(), 1);
        assert_eq!(outcome.typed_artifacts[0].name, "result");
    }

    #[test]
    fn provider_selection_matches_exact_backend_first() {
        let (_, mut exact_provider) = request("task-a", "node exact-provider.js".to_string());
        exact_provider.id = "exact-provider".to_string();
        exact_provider.backend = "requested-backend".to_string();
        exact_provider.extension_id = Some("other-extension".to_string());
        let (_, mut extension_provider) =
            request("task-b", "node extension-provider.js".to_string());
        extension_provider.id = "extension-provider".to_string();
        extension_provider.backend = "renamed-backend".to_string();
        extension_provider.extension_id = Some("requested-backend".to_string());

        let providers = [extension_provider, exact_provider];
        let selected = select_provider_by_backend(&providers, "requested-backend", None)
            .expect("provider selected");

        assert_eq!(selected.id, "exact-provider");
    }

    #[test]
    fn provider_selection_reports_exact_backend_selector_mismatch() {
        let (_, mut provider) = request("task-a", "node provider.js".to_string());
        provider.id = "example.synthetic-agent-task-executor".to_string();
        provider.backend = "synthetic-runtime".to_string();

        let providers = [provider];
        let resolution =
            resolve_provider_for_backend(&providers, "synthetic-runtime", Some("fast"));

        assert_eq!(
            resolution,
            ProviderResolution::SelectorMismatch {
                available_ids: vec!["example.synthetic-agent-task-executor".to_string()],
            }
        );
    }

    #[test]
    fn provider_selection_matches_unique_extension_alias() {
        let (_, mut provider) = request("task-a", "node provider.js".to_string());
        provider.id = "extension-a.provider".to_string();
        provider.backend = "renamed-backend".to_string();
        provider.extension_id = Some("extension-a".to_string());

        let providers = [provider];
        let selected =
            select_provider_by_backend(&providers, "extension-a", None).expect("provider selected");

        assert_eq!(selected.backend, "renamed-backend");
    }

    #[test]
    fn provider_selection_rejects_ambiguous_extension_alias() {
        let (_, mut provider_a) = request("task-a", "node provider-a.js".to_string());
        provider_a.id = "provider-a".to_string();
        provider_a.backend = "renamed-backend".to_string();
        provider_a.extension_id = Some("extension-a".to_string());
        let (_, mut provider_b) = request("task-b", "node provider-b.js".to_string());
        provider_b.id = "provider-b".to_string();
        provider_b.backend = "fixture".to_string();
        provider_b.extension_id = Some("extension-a".to_string());

        assert!(
            select_provider_by_backend(&[provider_a, provider_b], "extension-a", None).is_none()
        );
    }

    #[test]
    fn provider_selection_applies_selector_to_unique_extension_alias() {
        let (_, mut provider) = request("task-a", "node provider.js".to_string());
        provider.id = "selected-provider".to_string();
        provider.backend = "renamed-backend".to_string();
        provider.extension_id = Some("extension-a".to_string());

        assert!(
            select_provider_by_backend(&[provider.clone()], "extension-a", Some("missing"))
                .is_none()
        );
        assert_eq!(
            select_provider_by_backend(&[provider], "extension-a", Some("selected-provider"))
                .expect("provider selected")
                .id,
            "selected-provider"
        );
    }

    #[test]
    fn provider_manifest_parses_role_aliases() {
        let provider: AgentTaskExecutorProvider = serde_json::from_value(json!({
            "schema": "homeboy/agent-task-executor-provider/v1",
            "id": "custom.provider",
            "backend": "custom",
            "default_backend": true,
            "command": "custom-agent-task",
            "request_schema": AGENT_TASK_REQUEST_SCHEMA,
            "outcome_schema": AGENT_TASK_OUTCOME_SCHEMA,
            "role_aliases": {
                "artifact_kinds": {
                    "patch": ["custom-patch"]
                },
                "artifact_filenames": {
                    "preflight_evidence": ["*-preflight.json"]
                },
                "outputs": {
                    "provider_run_result": ["custom_run_result"]
                },
                "metadata": {
                    "provider_run_result": ["customRunResult"]
                }
            }
        }))
        .expect("provider manifest");

        assert!(provider.default_backend);
        assert!(provider
            .role_aliases
            .artifact_kind_matches_role("patch", "custom-patch"));
        assert!(provider
            .role_aliases
            .artifact_filename_matches_role("preflight_evidence", "runner-preflight.json"));
        assert_eq!(
            provider
                .role_aliases
                .output_aliases_for_role("provider_run_result"),
            vec!["custom_run_result"]
        );
    }

    #[test]
    fn provider_command_receives_canonical_artifact_declarations() {
        let command = format!(
            "node {}",
            script(
                r#"
const fs = require('fs');
const input = JSON.parse(fs.readFileSync(0, 'utf8'));
process.stdout.write(JSON.stringify({
  schema: 'homeboy/agent-task-outcome/v1',
  task_id: input.task_id,
  status: 'succeeded',
  artifacts: [],
  typed_artifacts: [],
  evidence_refs: [],
  diagnostics: [],
  outputs: { artifact_declarations: input.artifact_declarations },
  metadata: null
}));
"#
            )
        );
        let (mut request, provider) = request("task-artifact-normalization", command);
        request.expected_artifacts = vec!["patch".to_string()];

        let outcome = run_provider_command(&request, &provider);

        assert_eq!(outcome.status, AgentTaskOutcomeStatus::Succeeded);
        assert_eq!(outcome.outputs["artifact_declarations"][0]["name"], "patch");
        assert_eq!(
            outcome.outputs["artifact_declarations"][0]["required"],
            true
        );
    }

    #[test]
    fn provider_command_argv_preserves_extension_and_runtime_paths_with_spaces() {
        let temp = tempfile::tempdir().expect("tempdir");
        let extension_dir = temp.path().join("extension path with spaces");
        let runtime_dir = temp.path().join("runtime path with spaces");
        fs::create_dir_all(&extension_dir).expect("extension dir");
        fs::create_dir_all(&runtime_dir).expect("runtime dir");
        fs::write(
            runtime_dir.join("provider.js"),
            r#"
const fs = require('fs');
const input = JSON.parse(fs.readFileSync(0, 'utf8'));
process.stdout.write(JSON.stringify({
  schema: 'homeboy/agent-task-outcome/v1',
  task_id: input.task_id,
  status: process.argv[2] === '--extension' && process.argv[3].includes('extension path with spaces') ? 'succeeded' : 'failed',
  summary: process.argv.slice(2).join('|')
}));
"#,
        )
        .expect("provider script");
        let (request, mut provider) = request("task-argv-spaces", "legacy unused".to_string());
        provider.extension_path = Some(extension_dir.to_string_lossy().to_string());
        provider.runtime_path = Some(runtime_dir.to_string_lossy().to_string());
        provider.command_argv = vec![
            "node".to_string(),
            "{{runtime_path}}/provider.js".to_string(),
            "--extension".to_string(),
            "{{extension_path}}".to_string(),
        ];

        let outcome = run_provider_command(&request, &provider);

        assert_eq!(outcome.status, AgentTaskOutcomeStatus::Succeeded);
        assert!(outcome
            .summary
            .as_deref()
            .unwrap_or_default()
            .contains("extension path with spaces"));
    }

    #[test]
    fn provider_invocation_argv_and_cwd_preserve_paths_with_spaces() {
        let temp = tempfile::tempdir().expect("tempdir");
        let runtime_dir = temp.path().join("runtime path with spaces");
        fs::create_dir_all(&runtime_dir).expect("runtime dir");
        fs::write(
            runtime_dir.join("provider.js"),
            r#"
const fs = require('fs');
const input = JSON.parse(fs.readFileSync(0, 'utf8'));
process.stdout.write(JSON.stringify({
  schema: 'homeboy/agent-task-outcome/v1',
  task_id: input.task_id,
  status: process.cwd().includes('runtime path with spaces') ? 'succeeded' : 'failed',
  summary: process.cwd()
}));
"#,
        )
        .expect("provider script");
        let (request, mut provider) = request("task-invocation-cwd", "legacy unused".to_string());
        provider.runtime_path = Some(runtime_dir.to_string_lossy().to_string());
        provider.invocation.argv = vec!["node".to_string(), "provider.js".to_string()];
        provider.invocation.cwd = Some("{{runtime_path}}".to_string());

        let outcome = run_provider_command(&request, &provider);

        assert_eq!(outcome.status, AgentTaskOutcomeStatus::Succeeded);
        assert!(outcome
            .summary
            .as_deref()
            .unwrap_or_default()
            .contains("runtime path with spaces"));
    }

    #[test]
    fn provider_manifest_parses_runner_and_dependency_contracts() {
        let provider: AgentTaskExecutorProvider = serde_json::from_value(json!({
            "schema": "homeboy/agent-task-executor-provider/v1",
            "id": "custom.provider",
            "backend": "custom",
            "command": "custom-agent-task",
            "request_schema": AGENT_TASK_REQUEST_SCHEMA,
            "outcome_schema": AGENT_TASK_OUTCOME_SCHEMA,
            "runner_readiness": [{
                "id": "custom.runtime_cache",
                "label": "Custom runtime cache",
                "secret_env": ["CUSTOM_RUNTIME_TOKEN"],
                "env_path": { "env": ["CUSTOM_RUNTIME_BIN"], "revision": true },
                "remediation": "Refresh the custom runtime cache."
            }],
            "dependency_failure_patterns": [{
                "id": "custom.prepared_dependency",
                "label": "Custom prepared dependency",
                "path_contains": "prepared-dependencies/",
                "error_contains_any": ["enoent", "no such file or directory"],
                "remediation": "Refresh prepared dependencies."
            }]
        }))
        .expect("provider manifest");

        assert_eq!(provider.runner_readiness[0].id, "custom.runtime_cache");
        assert_eq!(
            provider.runner_readiness[0].secret_env,
            vec!["CUSTOM_RUNTIME_TOKEN"]
        );
        assert_eq!(
            provider.runner_readiness[0].env_path.as_ref().unwrap().env,
            vec!["CUSTOM_RUNTIME_BIN"]
        );
        assert_eq!(
            provider.dependency_failure_patterns[0].path_contains,
            "prepared-dependencies/"
        );
    }

    #[test]
    fn default_backend_ignores_provider_declaration() {
        let (_request, mut provider_a) = request("task-a", "node provider-a.js".to_string());
        provider_a.backend = "first".to_string();
        let (_request, mut provider_b) = request("task-b", "node provider-b.js".to_string());
        provider_b.backend = "preferred".to_string();
        provider_b.default_backend = true;

        let executor =
            ExtensionProviderAgentTaskExecutor::with_providers(vec![provider_a, provider_b]);

        crate::test_support::with_isolated_home(|_| {
            assert_eq!(executor.default_backend().unwrap(), None);
        });
    }

    #[test]
    fn default_backend_uses_global_config_policy() {
        crate::test_support::with_isolated_home(|_| {
            defaults::save_config(&defaults::HomeboyConfig {
                agent_task: defaults::AgentTaskConfig {
                    default_backend: Some("configured".to_string()),
                    ..defaults::AgentTaskConfig::default()
                },
                ..defaults::HomeboyConfig::default()
            })
            .expect("config saved");

            assert_eq!(default_backend().unwrap().as_deref(), Some("configured"));
        });
    }

    #[test]
    fn default_backend_uses_extension_policy() {
        crate::test_support::with_isolated_home(|home| {
            defaults::save_config(&defaults::HomeboyConfig {
                agent_task: defaults::AgentTaskConfig {
                    default_backend: Some("global-policy".to_string()),
                    ..defaults::AgentTaskConfig::default()
                },
                ..defaults::HomeboyConfig::default()
            })
            .expect("config saved");
            let extension_dir = home
                .path()
                .join(".config/homeboy/extensions/runtime-extension");
            std::fs::create_dir_all(&extension_dir).expect("extension dir");
            std::fs::write(
                extension_dir.join("runtime-extension.json"),
                json!({
                    "name": "Runtime Extension",
                    "version": "1.0.0",
                    "agent_task": { "default_backend": "extension-policy" }
                })
                .to_string(),
            )
            .expect("extension manifest");

            assert_eq!(
                default_backend().unwrap().as_deref(),
                Some("extension-policy")
            );
        });
    }

    #[test]
    fn default_backend_rejects_ambiguous_extension_policy() {
        crate::test_support::with_isolated_home(|home| {
            for (id, backend) in [("runtime-a", "backend-a"), ("runtime-b", "backend-b")] {
                let extension_dir = home.path().join(format!(".config/homeboy/extensions/{id}"));
                std::fs::create_dir_all(&extension_dir).expect("extension dir");
                std::fs::write(
                    extension_dir.join(format!("{id}.json")),
                    json!({
                        "name": id,
                        "version": "1.0.0",
                        "agent_task": { "default_backend": backend }
                    })
                    .to_string(),
                )
                .expect("extension manifest");
            }

            let error = default_backend().expect_err("ambiguous policy should fail");
            assert!(error.message.contains("ambiguous"));
        });
    }

    #[test]
    fn default_backend_reads_component_scoped_extension_policy() {
        let mut component = component::Component::new(
            "fixture".to_string(),
            "/tmp/fixture".to_string(),
            String::new(),
            None,
        );
        component.extensions = Some(std::collections::HashMap::from([(
            "runtime-extension".to_string(),
            component::ScopedExtensionConfig {
                settings: std::collections::HashMap::from([(
                    "agent_task".to_string(),
                    json!({ "default_backend": "component-policy" }),
                )]),
                ..component::ScopedExtensionConfig::default()
            },
        )]));

        assert_eq!(
            component_default_backend(&component).as_deref(),
            Some("component-policy")
        );
    }

    #[test]
    fn default_backend_ignores_provider_manifest_default_backend() {
        crate::test_support::with_isolated_home(|home| {
            let runtime_dir = home
                .path()
                .join(".config/homeboy/agent-runtimes/standalone-runtime");
            std::fs::create_dir_all(&runtime_dir).expect("runtime dir");
            std::fs::write(
                runtime_dir.join("standalone-runtime.json"),
                json!({
                    "schema": agent_runtime_manifest::AGENT_RUNTIME_MANIFEST_SCHEMA,
                    "id": "standalone-runtime",
                    "agent_task_executors": [{
                        "schema": "homeboy/agent-task-executor-provider/v1",
                        "id": "runtime.provider",
                        "backend": "runtime-default",
                        "default_backend": true,
                        "command": "runtime-provider",
                        "request_schema": AGENT_TASK_REQUEST_SCHEMA,
                        "outcome_schema": AGENT_TASK_OUTCOME_SCHEMA
                    }]
                })
                .to_string(),
            )
            .expect("runtime manifest");

            assert_eq!(default_backend().unwrap(), None);
        });
    }

    #[test]
    fn default_backend_is_absent_without_provider_declaration() {
        crate::test_support::with_isolated_home(|_| {
            let (_request, mut provider_a) = request("task-a", "node provider-a.js".to_string());
            provider_a.backend = "first".to_string();
            let (_request, mut provider_b) = request("task-b", "node provider-b.js".to_string());
            provider_b.backend = "second".to_string();

            let executor =
                ExtensionProviderAgentTaskExecutor::with_providers(vec![provider_a, provider_b]);

            assert_eq!(executor.default_backend().unwrap(), None);
        });
    }

    #[test]
    fn provider_command_interpolates_runtime_path_separately_from_extension_path() {
        let (_, mut provider) = request(
            "task-a",
            "{{runtime_path}}/bin/provider --extension {{extension_path}}".to_string(),
        );
        provider.extension_path = Some("/extensions/project-type".to_string());
        provider.runtime_path = Some("/agent-runtimes/example".to_string());

        assert_eq!(
            render_provider_command_display(&provider),
            "/agent-runtimes/example/bin/provider --extension /extensions/project-type"
        );
    }

    #[test]
    fn provider_runner_secret_env_contracts_are_applied_to_selected_plan_tasks() {
        let (mut request_a, mut provider_a) = request("task-a", "node provider-a.js".to_string());
        request_a.executor.backend = "provider-a".to_string();
        provider_a.backend = "provider-a".to_string();
        provider_a.runner_readiness = vec![AgentTaskProviderRunnerReadiness {
            id: "provider-a.auth".to_string(),
            label: "Provider A auth".to_string(),
            secret_env: vec!["PROVIDER_A_TOKEN".to_string()],
            env_path: None,
            executable: None,
            remediation: Some("Configure provider A auth.".to_string()),
            extra: BTreeMap::new(),
        }];
        let (mut request_b, mut provider_b) = request("task-b", "node provider-b.js".to_string());
        request_b.executor.backend = "provider-b".to_string();
        request_b.executor.secret_env = vec!["EXPLICIT_SECRET".to_string()];
        provider_b.backend = "provider-b".to_string();
        provider_b.runner_readiness = vec![AgentTaskProviderRunnerReadiness {
            id: "provider-b.auth".to_string(),
            label: "Provider B auth".to_string(),
            secret_env: vec![
                "PROVIDER_B_TOKEN".to_string(),
                "EXPLICIT_SECRET".to_string(),
            ],
            env_path: None,
            executable: None,
            remediation: None,
            extra: BTreeMap::new(),
        }];
        let mut plan = AgentTaskPlan::new("plan-a", vec![request_a, request_b]);

        apply_provider_runner_secret_env_contracts_with_providers(
            &mut plan,
            &[provider_a, provider_b],
        );

        assert_eq!(
            plan.tasks[0].executor.secret_env,
            vec!["PROVIDER_A_TOKEN".to_string()]
        );
        assert_eq!(
            plan.tasks[1].executor.secret_env,
            vec![
                "EXPLICIT_SECRET".to_string(),
                "PROVIDER_B_TOKEN".to_string()
            ]
        );
    }

    #[test]
    fn discovers_agent_task_providers_from_agent_runtime_manifests() {
        crate::test_support::with_isolated_home(|home| {
            let runtime_dir = home
                .path()
                .join(".config/homeboy/agent-runtimes/custom-runtime");
            fs::create_dir_all(&runtime_dir).expect("runtime dir");
            fs::write(
                runtime_dir.join("custom-runtime.json"),
                serde_json::to_string(&json!({
                    "schema": agent_runtime_manifest::AGENT_RUNTIME_MANIFEST_SCHEMA,
                    "id": "custom-runtime",
                    "name": "Custom Runtime",
                    "version": "1.0.0",
                    "agent_task_executors": [{
                        "schema": "homeboy/agent-task-executor-provider/v1",
                        "id": "custom.runtime.executor",
                        "backend": "custom",
                        "command": "node {{runtime_path}}/runner.cjs",
                        "request_schema": AGENT_TASK_REQUEST_SCHEMA,
                        "outcome_schema": AGENT_TASK_OUTCOME_SCHEMA
                    }]
                }))
                .unwrap(),
            )
            .expect("runtime manifest");

            let providers = discover_agent_task_executor_providers();

            assert_eq!(providers.len(), 1);
            assert_eq!(providers[0].id, "custom.runtime.executor");
            assert_eq!(providers[0].runtime_id.as_deref(), Some("custom-runtime"));
            assert_eq!(
                providers[0].runtime_path.as_deref(),
                Some(runtime_dir.to_string_lossy().as_ref())
            );
            assert_eq!(
                render_provider_command_display(&providers[0]),
                format!("node {}/runner.cjs", runtime_dir.display())
            );
        });
    }

    #[test]
    fn provider_command_env_includes_runtime_identity() {
        let (request, mut provider) =
            request("task-a", "node {{runtime_path}}/provider.js".to_string());
        provider.runtime_id = Some("custom-runtime".to_string());
        provider.runtime_path = Some("/tmp/custom-runtime".to_string());

        let env = provider_command_env(&request, &provider).expect("provider env");

        assert!(env.contains(&(
            "HOMEBOY_AGENT_RUNTIME_ID".to_string(),
            "custom-runtime".to_string()
        )));
        assert!(env.contains(&(
            "HOMEBOY_AGENT_RUNTIME_PATH".to_string(),
            "/tmp/custom-runtime".to_string()
        )));
        assert_eq!(
            render_provider_command_display(&provider),
            "node /tmp/custom-runtime/provider.js"
        );
    }

    #[test]
    fn provider_command_env_injects_declared_executable_candidate() {
        let temp = tempfile::tempdir().expect("tempdir");
        let tool = temp.path().join("provider-tool");
        fs::write(&tool, "#!/bin/sh\nexit 0\n").expect("write tool");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = fs::metadata(&tool).expect("tool metadata").permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&tool, permissions).expect("chmod tool");
        }
        let (request, mut provider) = request("task-a", "node provider.js".to_string());
        provider.runner_readiness = vec![AgentTaskProviderRunnerReadiness {
            id: "provider.tool".to_string(),
            label: "Provider tool".to_string(),
            secret_env: Vec::new(),
            env_path: None,
            executable: Some(AgentTaskProviderExecutableReadiness {
                env: vec!["HOMEBOY_TEST_PROVIDER_TOOL".to_string()],
                candidates: vec![tool.to_string_lossy().to_string()],
                version_command: vec!["--version".to_string()],
                install_hint: Some("Install provider-tool.".to_string()),
                extra: BTreeMap::new(),
            }),
            remediation: None,
            extra: BTreeMap::new(),
        }];
        std::env::remove_var("HOMEBOY_TEST_PROVIDER_TOOL");

        let env = provider_command_env(&request, &provider).expect("provider env");

        assert!(env.contains(&(
            "HOMEBOY_TEST_PROVIDER_TOOL".to_string(),
            tool.to_string_lossy().to_string()
        )));
    }

    #[test]
    fn provider_command_env_prefers_declared_executable_env_value() {
        let (request, mut provider) = request("task-a", "node provider.js".to_string());
        provider.runner_readiness = vec![AgentTaskProviderRunnerReadiness {
            id: "provider.tool".to_string(),
            label: "Provider tool".to_string(),
            secret_env: Vec::new(),
            env_path: None,
            executable: Some(AgentTaskProviderExecutableReadiness {
                env: vec!["HOMEBOY_TEST_PROVIDER_TOOL_ENV".to_string()],
                candidates: vec!["definitely-missing-provider-tool".to_string()],
                version_command: Vec::new(),
                install_hint: None,
                extra: BTreeMap::new(),
            }),
            remediation: None,
            extra: BTreeMap::new(),
        }];
        std::env::set_var("HOMEBOY_TEST_PROVIDER_TOOL_ENV", "/custom/provider-tool");

        let env = provider_command_env(&request, &provider).expect("provider env");

        std::env::remove_var("HOMEBOY_TEST_PROVIDER_TOOL_ENV");
        assert!(env.contains(&(
            "HOMEBOY_TEST_PROVIDER_TOOL_ENV".to_string(),
            "/custom/provider-tool".to_string()
        )));
    }

    #[test]
    fn provider_outcome_roles_normalize_from_declared_aliases() {
        let (_, mut provider) = request("task-a", "node provider.js".to_string());
        provider.role_aliases = serde_json::from_value(json!({
            "artifact_kinds": {
                "patch": ["custom-patch"]
            },
            "outputs": {
                "provider_run_result": ["custom_run_result"]
            }
        }))
        .expect("role aliases");
        let patch_path = std::env::temp_dir().join("custom.patch");
        fs::write(&patch_path, "diff --git a/a b/a\n").expect("patch");
        let mut outcome = AgentTaskOutcome {
            schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
            task_id: "task-a".to_string(),
            status: AgentTaskOutcomeStatus::Succeeded,
            summary: None,
            failure_classification: None,
            artifacts: vec![fixture_artifact(
                "patch",
                "custom-patch",
                &patch_path,
                Some("text/x-patch"),
            )],
            typed_artifacts: Vec::new(),
            evidence_refs: Vec::new(),
            diagnostics: Vec::new(),
            outputs: json!({
                "custom_run_result": {
                    "run_id": "custom-run-1"
                }
            }),
            workflow: None,
            follow_up: None,
            metadata: Value::Null,
        };

        normalize_provider_outcome_roles(&mut outcome, &provider);

        assert_eq!(outcome.artifacts[0].kind, "patch");
        assert_eq!(
            outcome.artifacts[0].metadata["provider_kind"],
            "custom-patch"
        );
        assert_eq!(
            outcome.outputs["provider_run_result"]["run_id"],
            "custom-run-1"
        );
        assert_eq!(
            outcome.outputs["custom_run_result"]["run_id"],
            "custom-run-1"
        );
    }

    fn failed_outcome_with_run_result(run_result: Value) -> AgentTaskOutcome {
        AgentTaskOutcome {
            schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
            task_id: "cook-conductor".to_string(),
            status: AgentTaskOutcomeStatus::Failed,
            summary: Some("Provider agent task failed.".to_string()),
            failure_classification: Some(AgentTaskFailureClassification::ExecutionFailed),
            artifacts: Vec::new(),
            typed_artifacts: Vec::new(),
            evidence_refs: Vec::new(),
            diagnostics: Vec::new(),
            outputs: json!({ "provider_run_result": run_result }),
            workflow: None,
            follow_up: None,
            metadata: Value::Null,
        }
    }

    #[test]
    fn empty_failed_run_result_surfaces_explanatory_diagnostic() {
        // Mirrors the #4105 repro: a failed run-result that is an empty shell.
        let mut outcome = failed_outcome_with_run_result(json!({
            "schema": "example-provider/agent-task-run-result/v1",
            "status": "failed",
            "failure_classification": "runtime",
            "artifacts": [],
            "diagnostics": [],
            "metadata": {
                "provider_error": {},
                "run_id": "",
                "run_status": "",
                "runtime_id": "",
                "runtime_status": ""
            },
            "refs": {
                "artifact_bundles": [],
                "changed_files": [],
                "logs": [],
                "patches": [],
                "runtimes": [],
                "transcripts": []
            }
        }));

        surface_provider_run_result_diagnostics(&mut outcome);

        assert_eq!(
            outcome.diagnostics.len(),
            1,
            "an empty failed run-result must still produce one reviewer-safe diagnostic"
        );
        assert_eq!(outcome.diagnostics[0].class, "provider.run_result_empty");
        assert!(outcome.diagnostics[0]
            .message
            .contains("no provider runtime or session"));
    }

    #[test]
    fn populated_failed_run_result_surfaces_error_identity_and_refs() {
        let mut outcome = failed_outcome_with_run_result(json!({
            "schema": "example-provider/agent-task-run-result/v1",
            "status": "failed",
            "diagnostics": [
                { "class": "provider.api_error", "message": "runtime provisioning rejected" }
            ],
            "metadata": {
                "provider_error": { "code": "E_RUNTIME", "message": "quota exceeded" },
                "run_id": "run-123",
                "run_status": "errored",
                "runtime_id": "rt-456",
                "runtime_status": "failed"
            },
            "refs": {
                "logs": ["https://provider.example/logs/run-123"],
                "transcripts": [{ "uri": "https://provider.example/transcripts/rt-456" }],
                "artifact_bundles": []
            }
        }));

        surface_provider_run_result_diagnostics(&mut outcome);

        // The provider's own diagnostic is lifted up.
        assert!(outcome
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.class == "provider.api_error"));
        // The structured identity becomes an actionable diagnostic.
        let identity = outcome
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.class == "provider.run_result_failed")
            .expect("identity diagnostic surfaced");
        assert!(identity.message.contains("quota exceeded"));
        assert!(identity.message.contains("run_id=run-123"));
        assert!(identity.message.contains("runtime_id=rt-456"));
        // provider_error is mirrored onto outcome metadata.
        assert_eq!(
            outcome.metadata["provider_error"]["code"],
            json!("E_RUNTIME")
        );
        // Log + transcript refs become evidence refs.
        assert!(outcome
            .evidence_refs
            .iter()
            .any(|reference| reference.kind == "provider-log"
                && reference.uri == "https://provider.example/logs/run-123"));
        assert!(outcome
            .evidence_refs
            .iter()
            .any(|reference| reference.kind == "provider-transcript"
                && reference.uri == "https://provider.example/transcripts/rt-456"));
        // The empty-shell guard must NOT fire when real evidence exists.
        assert!(outcome
            .diagnostics
            .iter()
            .all(|diagnostic| diagnostic.class != "provider.run_result_empty"));
    }

    #[test]
    fn succeeded_run_result_is_not_mined_for_failure_diagnostics() {
        let mut outcome = failed_outcome_with_run_result(json!({
            "status": "succeeded",
            "metadata": { "run_id": "run-999" }
        }));
        // Even though the outcome status is failed, a succeeded run-result is
        // left untouched (the failure cause is elsewhere).
        surface_provider_run_result_diagnostics(&mut outcome);
        assert!(outcome.diagnostics.is_empty());
    }

    #[test]
    fn non_failure_outcome_skips_run_result_mining() {
        let mut outcome = failed_outcome_with_run_result(json!({
            "status": "failed",
            "metadata": { "provider_error": { "message": "boom" } }
        }));
        outcome.status = AgentTaskOutcomeStatus::Succeeded;
        surface_provider_run_result_diagnostics(&mut outcome);
        assert!(outcome.diagnostics.is_empty());
        assert!(outcome.metadata.get("provider_error").is_none());
    }

    #[test]
    fn provider_workspace_materialization_declares_cwd_git_checkout_requirement() {
        let (_request, mut provider) = request("task-a", "node provider-a.js".to_string());
        provider.workspace_materialization = Some(AgentTaskProviderWorkspaceMaterialization {
            cwd: Some("git_checkout".to_string()),
            requires_git: None,
            write_scope: None,
            artifact_paths: Vec::new(),
            spec: None,
            mounts: Vec::new(),
            apply_back: AgentTaskRuntimeApplyBack::default(),
            extra: BTreeMap::new(),
        });

        assert!(provider_requires_cwd_git_checkout_with_providers(
            &[provider],
            "test",
            None
        ));
    }

    #[test]
    fn provider_default_secret_sources_resolve_required_env_without_duplicate_mapping() {
        crate::test_support::with_isolated_home(|_| {
            let temp = tempfile::tempdir().expect("tempdir");
            let auth_path = temp.path().join("provider-auth.json");
            fs::write(
                &auth_path,
                json!({
                    "tokens": {
                        "access_token": "provider-owned-access-token",
                        "refresh_token": "provider-owned-refresh-token"
                    }
                })
                .to_string(),
            )
            .expect("write auth");
            let (mut request, mut provider) = request("task-a", "node provider-a.js".to_string());
            request.executor.config = json!({ "provider": "example-oauth" });
            request.executor.secret_env = vec![
                "EXAMPLE_PROVIDER_ACCESS_TOKEN".to_string(),
                "EXAMPLE_PROVIDER_REFRESH_TOKEN".to_string(),
            ];
            provider.provider_defaults.insert(
                "example-oauth".to_string(),
                json!({
                    "secret_env": request.executor.secret_env,
                    "secret_env_sources": {
                        "EXAMPLE_PROVIDER_ACCESS_TOKEN": {
                            "source": "json-file",
                            "path": auth_path,
                            "field": "tokens.access_token"
                        },
                        "EXAMPLE_PROVIDER_REFRESH_TOKEN": {
                            "source": "json-file",
                            "path": auth_path,
                            "field": "tokens.refresh_token"
                        }
                    }
                }),
            );

            let env = provider_command_env(&request, &provider).expect("provider env resolves");

            assert!(env.contains(&(
                "EXAMPLE_PROVIDER_ACCESS_TOKEN".to_string(),
                "provider-owned-access-token".to_string()
            )));
            let rendered =
                serde_json::to_string(&provider_secret_sources(&provider, Some(&request)))
                    .expect("sources json");
            assert!(!rendered.contains("provider-owned-access-token"));
        });
    }

    #[test]
    fn provider_secret_sources_for_providers_include_default_json_sources() {
        let (_request, mut provider) = request("task-a", "node provider-a.js".to_string());
        provider.provider_defaults.insert(
            "example-oauth".to_string(),
            json!({
                "secret_env": ["EXAMPLE_PROVIDER_ACCESS_TOKEN"],
                "secret_env_sources": {
                    "EXAMPLE_PROVIDER_ACCESS_TOKEN": {
                        "source": "json-file",
                        "path": "~/.example-provider/auth.json",
                        "field": "tokens.access_token"
                    }
                }
            }),
        );

        let sources = provider_secret_sources_for_providers(&[provider]);

        let source = sources
            .get("EXAMPLE_PROVIDER_ACCESS_TOKEN")
            .expect("provider default source discovered");
        assert_eq!(source.source, "json-file");
        assert_eq!(
            source.path.as_deref(),
            Some("~/.example-provider/auth.json")
        );
        assert_eq!(source.field.as_deref(), Some("tokens.access_token"));
    }

    #[test]
    fn provider_default_secret_sources_accept_nested_json_sources() {
        crate::test_support::with_isolated_home(|_| {
            let temp = tempfile::tempdir().expect("tempdir");
            let auth_path = temp.path().join("provider-auth.json");
            fs::write(
                &auth_path,
                json!({
                    "provider": {
                        "access": "provider-access-token",
                        "refresh": "provider-refresh-token",
                        "expires": 12345
                    }
                })
                .to_string(),
            )
            .expect("write auth");
            let auth_path = auth_path.to_string_lossy().to_string();
            let (mut request, mut provider) = request("task-a", "node provider-a.js".to_string());
            request.executor.config = json!({ "provider": "example-oauth" });
            request.executor.secret_env = vec![
                "EXAMPLE_PROVIDER_ACCESS_TOKEN".to_string(),
                "EXAMPLE_PROVIDER_REFRESH_TOKEN".to_string(),
                "EXAMPLE_PROVIDER_EXPIRES_AT".to_string(),
            ];
            provider.provider_defaults.insert(
                "example-oauth".to_string(),
                json!({
                    "secret_env": [
                        "EXAMPLE_PROVIDER_ACCESS_TOKEN",
                        "EXAMPLE_PROVIDER_REFRESH_TOKEN",
                        "EXAMPLE_PROVIDER_EXPIRES_AT"
                    ],
                    "secret_env_sources": {
                        "EXAMPLE_PROVIDER_ACCESS_TOKEN": {
                            "source": "json-file",
                            "path": auth_path.clone(),
                            "field": "provider.access"
                        },
                        "EXAMPLE_PROVIDER_REFRESH_TOKEN": {
                            "source": "json-file",
                            "path": auth_path.clone(),
                            "field": "provider.refresh"
                        },
                        "EXAMPLE_PROVIDER_EXPIRES_AT": {
                            "source": "json-file",
                            "path": auth_path.clone(),
                            "field": "provider.expires"
                        }
                    }
                }),
            );

            let env = provider_command_env(&request, &provider).expect("provider env resolves");

            assert!(env.contains(&(
                "EXAMPLE_PROVIDER_REFRESH_TOKEN".to_string(),
                "provider-refresh-token".to_string()
            )));
            assert!(env.contains(&(
                "EXAMPLE_PROVIDER_EXPIRES_AT".to_string(),
                "12345".to_string()
            )));
        });
    }

    #[test]
    fn provider_default_secret_sources_feed_secret_readiness_status() {
        crate::test_support::with_isolated_home(|_| {
            let temp = tempfile::tempdir().expect("tempdir");
            let auth_path = temp.path().join("provider-auth.json");
            fs::write(
                &auth_path,
                json!({
                    "tokens": {
                        "access_token": "provider-owned-access-token"
                    }
                })
                .to_string(),
            )
            .expect("write auth");
            let (_request, mut provider) = request("task-a", "node provider-a.js".to_string());
            provider.provider_defaults.insert(
                "example-oauth".to_string(),
                json!({
                    "secret_env_sources": {
                        "EXAMPLE_PROVIDER_ACCESS_TOKEN": {
                            "source": "json-file",
                            "path": auth_path,
                            "field": "tokens.access_token"
                        }
                    }
                }),
            );
            let fallback_sources = provider_secret_sources_for_providers(&[provider]);

            let status = crate::core::agent_task_secrets::secret_env_status_with_fallbacks(
                &["EXAMPLE_PROVIDER_ACCESS_TOKEN".to_string()],
                &fallback_sources,
            );

            assert_eq!(status.len(), 1);
            assert!(status[0].configured);
            assert_eq!(status[0].source, "json-file");
        });
    }

    #[test]
    fn provider_workspace_materialization_declares_requires_git_requirement() {
        let (_request, mut provider) = request("task-a", "node provider-a.js".to_string());
        provider.workspace_materialization = Some(AgentTaskProviderWorkspaceMaterialization {
            cwd: None,
            requires_git: Some(true),
            write_scope: Some("artifacts".to_string()),
            artifact_paths: vec![".homeboy/provider".to_string()],
            spec: None,
            mounts: Vec::new(),
            apply_back: AgentTaskRuntimeApplyBack::default(),
            extra: BTreeMap::new(),
        });

        assert!(provider_requires_cwd_git_checkout_with_providers(
            &[provider],
            "test",
            None
        ));
    }

    #[test]
    fn provider_apply_back_contract_declares_git_checkout_requirement() {
        let (_request, mut provider) = request("task-a", "node provider-a.js".to_string());
        provider.workspace_materialization = Some(AgentTaskProviderWorkspaceMaterialization {
            apply_back: AgentTaskRuntimeApplyBack {
                requires_git_checkout: Some(true),
                strategy: Some("mutation_artifacts".to_string()),
                mutation_artifacts: vec![AgentTaskRuntimeMutationArtifact {
                    name: "patch".to_string(),
                    path: "outputs.runtime.artifacts.patch".to_string(),
                    kind: Some("patch".to_string()),
                    semantic_key: Some("workspace.patch".to_string()),
                    apply_method: Some("git_apply".to_string()),
                }],
            },
            ..AgentTaskProviderWorkspaceMaterialization::default()
        });

        assert!(provider_requires_cwd_git_checkout_with_providers(
            &[provider],
            "test",
            None
        ));
    }

    #[test]
    fn provider_workspace_materialization_ignores_unselected_provider() {
        let (_request, mut provider) = request("task-a", "node provider-a.js".to_string());
        provider.workspace_materialization = Some(AgentTaskProviderWorkspaceMaterialization {
            cwd: Some("git_checkout".to_string()),
            requires_git: None,
            write_scope: None,
            artifact_paths: Vec::new(),
            spec: None,
            mounts: Vec::new(),
            apply_back: AgentTaskRuntimeApplyBack::default(),
            extra: BTreeMap::new(),
        });

        assert!(!provider_requires_cwd_git_checkout_with_providers(
            &[provider],
            "other",
            None
        ));
    }

    #[test]
    fn provider_workspace_materialization_exports_typed_mount_specs() {
        let (_request, mut provider) = request("task-a", "node provider-a.js".to_string());
        provider.workspace_materialization = Some(AgentTaskProviderWorkspaceMaterialization {
            cwd: Some("workspace".to_string()),
            mounts: vec![WorkspaceMountSpec {
                handle: Some("homeboy@fix-workspace-materialization-spec".to_string()),
                repo: Some("homeboy".to_string()),
                host_path: Some("/host/workspaces/homeboy@fix".to_string()),
                target_path: Some("/workspace/homeboy".to_string()),
                mode: Some("read_write".to_string()),
                materialization: Some("bind_mount".to_string()),
                metadata: json!({ "source": "fixture" }),
                extra: BTreeMap::new(),
            }],
            ..AgentTaskProviderWorkspaceMaterialization::default()
        });

        let exported = serde_json::to_value(&provider).expect("provider json");

        assert_eq!(
            exported["workspace_materialization"]["mounts"][0]["handle"],
            "homeboy@fix-workspace-materialization-spec"
        );
        assert_eq!(
            exported["workspace_materialization"]["mounts"][0]["target_path"],
            "/workspace/homeboy"
        );
        assert_eq!(
            exported["workspace_materialization"]["mounts"][0]["materialization"],
            "bind_mount"
        );
    }

    #[test]
    fn workspace_materialization_spec_validates_nested_mounts() {
        let materialization = AgentTaskProviderWorkspaceMaterialization {
            spec: Some(WorkspaceMaterializationSpec {
                materialization: Some("bind_mount".to_string()),
                mounts: vec![WorkspaceMountSpec {
                    host_path: Some("/tmp/homeboy".to_string()),
                    target_path: Some(" ".to_string()),
                    ..WorkspaceMountSpec::default()
                }],
                ..WorkspaceMaterializationSpec::default()
            }),
            mounts: vec![WorkspaceMountSpec {
                host_path: Some("/tmp/homeboy".to_string()),
                ..WorkspaceMountSpec::default()
            }],
            ..AgentTaskProviderWorkspaceMaterialization::default()
        };

        let errors = materialization.validate().expect_err("validation errors");

        assert_eq!(
            errors,
            vec![
                "spec.mounts[0].target_path must not be blank".to_string(),
                "mounts[0].target_path is required when host_path is set".to_string(),
            ]
        );
    }

    #[test]
    fn provider_secret_contracts_are_applied_generically() {
        let (mut request, mut provider) = request("task-a", "node provider-a.js".to_string());
        request.executor.config = json!({ "provider": "example-provider" });
        provider.secret_requirements = vec![
            AgentTaskProviderSecretRequirement {
                name: Some("REQUIRED_TOKEN".to_string()),
                required: Some(true),
                ..AgentTaskProviderSecretRequirement::default()
            },
            AgentTaskProviderSecretRequirement {
                name: Some("OPTIONAL_TOKEN".to_string()),
                required: Some(false),
                ..AgentTaskProviderSecretRequirement::default()
            },
        ];
        provider.secret_env_requirements = vec![AgentTaskProviderSecretEnvRequirement {
            env: vec!["EXAMPLE_PROVIDER_TOKEN".to_string()],
            when: Some(json!({
                "any": [
                    { "path": "executor.config.provider", "equals": "example-provider" },
                    { "path": "provider", "equals": "example-provider" }
                ]
            })),
            ..AgentTaskProviderSecretEnvRequirement::default()
        }];
        provider.provider_defaults.insert(
            "example-provider".to_string(),
            json!({ "secret_env": ["EXAMPLE_PROVIDER_REFRESH_TOKEN"] }),
        );
        let mut plan = AgentTaskPlan::new("plan-a", vec![request]);

        apply_provider_runner_secret_env_contracts_with_providers(&mut plan, &[provider]);

        assert_eq!(
            plan.tasks[0].executor.secret_env,
            vec![
                "EXAMPLE_PROVIDER_REFRESH_TOKEN".to_string(),
                "EXAMPLE_PROVIDER_TOKEN".to_string(),
                "REQUIRED_TOKEN".to_string(),
            ]
        );
    }

    #[test]
    fn scheduler_dispatches_extension_provider_command() {
        let command = format!(
            "node {}",
            script("let fs=require('fs'); let req=JSON.parse(fs.readFileSync(0,'utf8')); process.stdout.write(JSON.stringify({schema:'homeboy/agent-task-outcome/v1',task_id:req.task_id,status:'succeeded',summary:'ok',outputs:{issue_number:3447}}));")
        );
        let (request, provider) = request("task-a", command);
        let scheduler =
            AgentTaskScheduler::new(ExtensionProviderAgentTaskExecutor::with_providers(vec![
                provider,
            ]));

        let aggregate = scheduler.run(AgentTaskPlan::new("plan-a", vec![request]));

        assert_eq!(aggregate.totals.succeeded, 1);
        assert_eq!(
            aggregate.outcomes[0].status,
            AgentTaskOutcomeStatus::Succeeded
        );
        assert_eq!(aggregate.outcomes[0].outputs["issue_number"], json!(3447));
    }

    #[test]
    fn scheduler_reports_missing_extension_provider() {
        let (request, _provider) = request("task-missing-provider", "unused".to_string());
        let scheduler = AgentTaskScheduler::new(ExtensionProviderAgentTaskExecutor::default());

        let aggregate = scheduler.run(AgentTaskPlan::new("plan-missing-provider", vec![request]));

        assert_eq!(aggregate.totals.failed, 1);
        assert_eq!(
            aggregate.outcomes[0].failure_classification,
            Some(AgentTaskFailureClassification::CapabilityMissing)
        );
        assert_eq!(
            aggregate.outcomes[0].diagnostics[0].class,
            "agent_task.provider_missing"
        );
    }

    #[test]
    fn scheduler_reports_provider_selector_mismatch() {
        let (mut request, mut provider) = request("task-selector-mismatch", "unused".to_string());
        request.executor.backend = "synthetic-runtime".to_string();
        request.executor.selector = Some("fast".to_string());
        provider.id = "example.synthetic-agent-task-executor".to_string();
        provider.backend = "synthetic-runtime".to_string();
        let scheduler =
            AgentTaskScheduler::new(ExtensionProviderAgentTaskExecutor::with_providers(vec![
                provider,
            ]));

        let aggregate = scheduler.run(AgentTaskPlan::new("plan-selector-mismatch", vec![request]));

        assert_eq!(aggregate.totals.failed, 1);
        assert_eq!(
            aggregate.outcomes[0].diagnostics[0].class,
            "agent_task.provider_selector_mismatch"
        );
        assert_eq!(
            aggregate.outcomes[0].diagnostics[0].data["available_provider_ids"],
            json!(["example.synthetic-agent-task-executor"])
        );
    }

    #[test]
    fn scheduler_reports_missing_provider_capability() {
        let (mut request, provider) = request("task-missing-capability", "unused".to_string());
        request.executor.required_capabilities = vec!["workspace_write".to_string()];
        let scheduler =
            AgentTaskScheduler::new(ExtensionProviderAgentTaskExecutor::with_providers(vec![
                provider,
            ]));

        let aggregate = scheduler.run(AgentTaskPlan::new("plan-missing-capability", vec![request]));

        assert_eq!(aggregate.totals.failed, 1);
        assert_eq!(
            aggregate.outcomes[0].failure_classification,
            Some(AgentTaskFailureClassification::CapabilityMissing)
        );
        assert_eq!(
            aggregate.outcomes[0].diagnostics[0].class,
            "agent_task.capability_missing"
        );
        assert_eq!(
            aggregate.outcomes[0].diagnostics[0].data["missing_capabilities"],
            json!(["workspace_write"])
        );
    }

    #[test]
    fn scheduler_normalizes_malformed_provider_output() {
        let command = format!("node {}", script("process.stdout.write('{not json');"));
        let (request, provider) = request("task-malformed-provider", command);
        let scheduler =
            AgentTaskScheduler::new(ExtensionProviderAgentTaskExecutor::with_providers(vec![
                provider,
            ]));

        let aggregate = scheduler.run(AgentTaskPlan::new("plan-malformed-provider", vec![request]));

        assert_eq!(aggregate.totals.failed, 1);
        assert_eq!(
            aggregate.outcomes[0].failure_classification,
            Some(AgentTaskFailureClassification::Provider)
        );
        assert_eq!(
            aggregate.outcomes[0].diagnostics[0].class,
            "agent_task.provider_malformed_json"
        );
        assert_eq!(
            aggregate.outcomes[0].diagnostics[0].data["stdout"],
            "{not json"
        );
    }

    #[test]
    fn provider_preserves_structured_outcome_from_stderr_when_stdout_empty() {
        let command = format!(
            "node {}",
            script("let fs=require('fs'); let req=JSON.parse(fs.readFileSync(0,'utf8')); process.stderr.write('diagnostic prefix\\n' + JSON.stringify({schema:'homeboy/agent-task-outcome/v1',task_id:req.task_id,status:'failed',summary:'captured provider evidence',failure_classification:'provider',diagnostics:[{class:'sample_runtime.empty_data_packet_returned',message:'empty data packet returned',data:{typed_artifacts:{}}}]}));")
        );
        let (request, provider) = request("task-stderr-outcome", command);

        let outcome = run_provider_command(&request, &provider);

        assert_eq!(outcome.status, AgentTaskOutcomeStatus::Failed);
        assert_eq!(
            outcome.summary.as_deref(),
            Some("captured provider evidence")
        );
        assert_eq!(
            outcome.diagnostics[0].class,
            "sample_runtime.empty_data_packet_returned"
        );
        assert_eq!(outcome.diagnostics[0].data["typed_artifacts"], json!({}));
    }

    #[test]
    fn provider_timeout_returns_structured_outcome() {
        let command = format!("node {}", script("setInterval(() => {}, 1000);"));
        let (mut request, provider) = request("task-timeout", command);
        request.limits.timeout_ms = Some(50);
        let scheduler =
            AgentTaskScheduler::new(ExtensionProviderAgentTaskExecutor::with_providers(vec![
                provider,
            ]));

        let aggregate = scheduler.run(AgentTaskPlan::new("plan-timeout", vec![request]));

        assert_eq!(aggregate.totals.timed_out, 1);
        assert_eq!(
            aggregate.outcomes[0].failure_classification,
            Some(AgentTaskFailureClassification::Timeout)
        );
    }

    #[test]
    fn provider_can_return_timeout_payload_during_wrapper_grace() {
        let command = format!(
            "node {}",
            script("let fs=require('fs'); let req=JSON.parse(fs.readFileSync(0,'utf8')); setTimeout(()=>process.stdout.write(JSON.stringify({schema:'homeboy/agent-task-outcome/v1',task_id:req.task_id,status:'timeout',summary:'provider serialized timeout',failure_classification:'timeout',artifacts:[{schema:'homeboy/agent-task-artifact/v1',id:'timeout-evidence',kind:'provider-task-runner-preflight',path:'/tmp/timeout-evidence.json'}]})), 3050);")
        );
        let (mut request, provider) = request("task-timeout-payload", command);
        request.limits.timeout_ms = Some(3000);

        let outcome = run_provider_command(&request, &provider);

        assert_eq!(outcome.status, AgentTaskOutcomeStatus::Timeout);
        assert_eq!(
            outcome.summary.as_deref(),
            Some("provider serialized timeout")
        );
        assert_eq!(outcome.artifacts.len(), 1);
        assert_eq!(outcome.artifacts[0].id, "timeout-evidence");
    }

    #[test]
    fn provider_command_receives_executor_config_env() {
        let command = format!(
            "node {}",
            script("let fs=require('fs'); let req=JSON.parse(fs.readFileSync(0,'utf8')); let config=JSON.parse(process.env.HOMEBOY_AGENT_TASK_EXECUTOR_CONFIG_JSON); process.stdout.write(JSON.stringify({schema:'homeboy/agent-task-outcome/v1',task_id:req.task_id,status:config.marker==='configured'?'succeeded':'failed',summary:process.env.HOMEBOY_AGENT_TASK_PROVIDER_ID}));")
        );
        let (mut request, mut provider) = request("task-config", command);
        request.executor.config = json!({ "marker": "configured" });
        provider.extension_id = Some("wordpress".to_string());
        provider.extension_path = Some("/tmp/homeboy-extension".to_string());
        let scheduler =
            AgentTaskScheduler::new(ExtensionProviderAgentTaskExecutor::with_providers(vec![
                provider,
            ]));

        let aggregate = scheduler.run(AgentTaskPlan::new("plan-config", vec![request]));

        assert_eq!(aggregate.totals.succeeded, 1);
        assert_eq!(
            aggregate.outcomes[0].summary.as_deref(),
            Some("test.provider")
        );
    }

    #[test]
    fn provider_command_receives_declared_secret_env() {
        let secret_name = format!("HOMEBOY_TEST_AGENT_TASK_SECRET_{}", std::process::id());
        std::env::set_var(&secret_name, "hydrated-secret");
        let command = format!(
            "node {}",
            script(&format!(
                "let fs=require('fs'); let req=JSON.parse(fs.readFileSync(0,'utf8')); process.stdout.write(JSON.stringify({{schema:'homeboy/agent-task-outcome/v1',task_id:req.task_id,status:process.env.{secret_name}==='hydrated-secret'?'succeeded':'failed',summary:'checked'}}));"
            ))
        );
        let (mut request, provider) = request("task-secret-env", command);
        request.executor.secret_env = vec![secret_name.clone()];
        let scheduler =
            AgentTaskScheduler::new(ExtensionProviderAgentTaskExecutor::with_providers(vec![
                provider,
            ]));

        let aggregate = scheduler.run(AgentTaskPlan::new("plan-secret-env", vec![request]));

        assert_eq!(aggregate.totals.succeeded, 1);
        std::env::remove_var(secret_name);
    }

    #[test]
    fn provider_command_receives_canonical_secret_env_plan_without_values() {
        let secret_name = format!("HOMEBOY_TEST_AGENT_TASK_PLAN_SECRET_{}", std::process::id());
        std::env::set_var(&secret_name, "hydrated-secret");
        let command = format!(
            "node {}",
            script(&format!(
                "let fs=require('fs'); let req=JSON.parse(fs.readFileSync(0,'utf8')); let plan=JSON.parse(process.env.HOMEBOY_AGENT_TASK_SECRET_ENV_PLAN_JSON); let mapped=(plan.env_name_mapping['test.provider']||[]).includes('{secret_name}'); let configured=(plan.status||[]).some((item)=>item.name==='{secret_name}'&&item.configured===true&&item.source==='env'); let leaked=JSON.stringify(plan).includes('hydrated-secret'); process.stdout.write(JSON.stringify({{schema:'homeboy/agent-task-outcome/v1',task_id:req.task_id,status:mapped&&configured&&!leaked?'succeeded':'failed',summary:JSON.stringify(plan)}}));"
            ))
        );
        let (mut request, mut provider) = request("task-secret-env-plan", command);
        provider.runner_readiness = vec![AgentTaskProviderRunnerReadiness {
            id: "test.provider.auth".to_string(),
            label: "Test provider auth".to_string(),
            secret_env: vec![secret_name.clone()],
            env_path: None,
            executable: None,
            remediation: None,
            extra: BTreeMap::new(),
        }];
        request.executor.secret_env = vec![secret_name.clone()];
        let scheduler =
            AgentTaskScheduler::new(ExtensionProviderAgentTaskExecutor::with_providers(vec![
                provider,
            ]));

        let aggregate = scheduler.run(AgentTaskPlan::new("plan-secret-env-plan", vec![request]));

        assert_eq!(aggregate.totals.succeeded, 1);
        assert!(!aggregate.outcomes[0]
            .summary
            .as_deref()
            .unwrap_or_default()
            .contains("hydrated-secret"));
        std::env::remove_var(secret_name);
    }

    #[test]
    fn missing_declared_secret_env_fails_before_provider_spawn() {
        let secret_name = format!(
            "HOMEBOY_TEST_MISSING_AGENT_TASK_SECRET_{}",
            std::process::id()
        );
        std::env::remove_var(&secret_name);
        let command = format!(
            "node {}",
            script("throw new Error('provider should not run');")
        );
        let (mut request, provider) = request("task-missing-secret-env", command);
        request.executor.secret_env = vec![secret_name.clone()];
        let scheduler =
            AgentTaskScheduler::new(ExtensionProviderAgentTaskExecutor::with_providers(vec![
                provider,
            ]));

        let aggregate = scheduler.run(AgentTaskPlan::new("plan-missing-secret-env", vec![request]));

        assert_eq!(aggregate.totals.failed, 1);
        assert_eq!(
            aggregate.outcomes[0].failure_classification,
            Some(AgentTaskFailureClassification::InvalidInput)
        );
        assert_eq!(
            aggregate.outcomes[0].diagnostics[0].class,
            "agent_task.secret_env_missing"
        );
        assert_eq!(
            aggregate.outcomes[0].diagnostics[0].data["missing_secret_env"],
            json!([secret_name])
        );
    }

    #[test]
    fn fixture_backend_produces_deterministic_smoke_artifacts() {
        let artifact_root = tempfile::tempdir().expect("artifact root");
        let (mut request, _provider) = request("task-fixture", "unused".to_string());
        request.executor.backend = "fixture".to_string();
        request.executor.config = json!({
            "artifact_root": artifact_root.path().display().to_string(),
            "changed_file": "docs/smoke.md"
        });
        let scheduler = AgentTaskScheduler::new(ExtensionProviderAgentTaskExecutor::default());

        let aggregate = scheduler.run(AgentTaskPlan::new("plan-fixture", vec![request]));

        assert_eq!(aggregate.totals.succeeded, 1);
        let outcome = &aggregate.outcomes[0];
        assert_eq!(outcome.status, AgentTaskOutcomeStatus::Succeeded);
        assert!(outcome.artifacts.iter().any(
            |artifact| artifact.kind == "patch" && artifact.size_bytes.unwrap_or_default() > 0
        ));
        assert!(outcome
            .artifacts
            .iter()
            .any(|artifact| artifact.kind == "agent_result"));
        assert!(outcome
            .evidence_refs
            .iter()
            .any(|evidence| evidence.kind == "transcript"));
    }

    #[test]
    fn fixture_backend_classifies_empty_runtime_bundle() {
        let artifact_root = tempfile::tempdir().expect("artifact root");
        let (mut request, _provider) = request("task-empty-runtime", "unused".to_string());
        request.executor.backend = "fixture".to_string();
        request.executor.config = json!({
            "artifact_root": artifact_root.path().display().to_string(),
            "mode": "empty_runtime_bundle"
        });
        let scheduler = AgentTaskScheduler::new(ExtensionProviderAgentTaskExecutor::default());

        let aggregate = scheduler.run(AgentTaskPlan::new("plan-empty-runtime", vec![request]));

        assert_eq!(aggregate.totals.failed, 1);
        assert_eq!(
            aggregate.outcomes[0].diagnostics[0].class,
            "agent_task.fixture_empty_runtime_bundle"
        );
        assert!(aggregate.outcomes[0]
            .artifacts
            .iter()
            .any(|artifact| artifact.kind == "runtime_bundle"));
    }

    #[test]
    fn is_transient_provider_error_classifies_transient_and_permanent_text() {
        // Transient network/provider blips.
        assert!(is_transient_provider_error(
            "Network error ... cURL error 28: Operation timed out after 15000ms"
        ));
        assert!(is_transient_provider_error("connection reset by peer"));
        assert!(is_transient_provider_error("503 Service Unavailable"));
        assert!(is_transient_provider_error("HTTP 502 Bad Gateway"));
        assert!(is_transient_provider_error("429 Too Many Requests"));

        // Permanent failures must not be treated as transient.
        assert!(!is_transient_provider_error(
            "401 Unauthorized: invalid token"
        ));
        assert!(!is_transient_provider_error(
            "400 Bad Request: validation failed"
        ));
        assert!(!is_transient_provider_error("404 Not Found"));
        assert!(!is_transient_provider_error(
            "malformed JSON in provider output"
        ));
        assert!(!is_transient_provider_error(
            "provider output path /tmp/homeboy-500abc/stdout.json was malformed"
        ));
    }

    /// Node script that increments a counter file and emits a transient cURL-28
    /// provider error for the first `fail_until` attempts, then a success
    /// outcome. Used to prove transient retries recover.
    fn transient_then_success_script(state_path: &Path, fail_until: u32) -> String {
        let state = state_path.to_string_lossy().replace('\\', "\\\\");
        script(&format!(
            "let fs=require('fs'); let req=JSON.parse(fs.readFileSync(0,'utf8')); \
             let p='{state}'; let n=0; try {{ n=parseInt(fs.readFileSync(p,'utf8'))||0; }} catch(e) {{}} \
             n+=1; fs.writeFileSync(p, String(n)); \
             if (n <= {fail_until}) {{ \
               process.stdout.write(JSON.stringify({{schema:'homeboy/agent-task-outcome/v1',task_id:req.task_id,status:'provider_error',summary:'Network error ... cURL error 28: Operation timed out after 15000ms',failure_classification:'provider'}})); \
             }} else {{ \
               process.stdout.write(JSON.stringify({{schema:'homeboy/agent-task-outcome/v1',task_id:req.task_id,status:'succeeded',summary:'recovered'}})); \
             }}",
        ))
    }

    /// Node script that increments a counter file and always emits a permanent
    /// auth/validation provider error. Used to prove permanent errors fail fast.
    fn permanent_error_script(state_path: &Path) -> String {
        let state = state_path.to_string_lossy().replace('\\', "\\\\");
        script(&format!(
            "let fs=require('fs'); let req=JSON.parse(fs.readFileSync(0,'utf8')); \
             let p='{state}'; let n=0; try {{ n=parseInt(fs.readFileSync(p,'utf8'))||0; }} catch(e) {{}} \
             n+=1; fs.writeFileSync(p, String(n)); \
             process.stdout.write(JSON.stringify({{schema:'homeboy/agent-task-outcome/v1',task_id:req.task_id,status:'provider_error',summary:'401 Unauthorized: invalid token',failure_classification:'provider'}}));",
        ))
    }

    fn unique_state_path(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "homeboy-transient-retry-{}-{}-{}.count",
            label,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default()
        ))
    }

    #[test]
    fn provider_retries_transient_error_then_succeeds() {
        let state_path = unique_state_path("recover");
        let _ = fs::remove_file(&state_path);
        let command = format!("node {}", transient_then_success_script(&state_path, 2));
        let (request, provider) = request("task-transient-recover", command);

        let outcome = run_provider_command(&request, &provider);

        assert_eq!(
            outcome.status,
            AgentTaskOutcomeStatus::Succeeded,
            "transient blip should be retried until it recovers"
        );
        let attempts: u32 = fs::read_to_string(&state_path)
            .ok()
            .and_then(|raw| raw.trim().parse().ok())
            .unwrap_or_default();
        assert_eq!(attempts, 3, "two transient failures plus one success");
        assert!(
            outcome
                .diagnostics
                .iter()
                .any(|d| d.class == "agent_task.provider_transient_retry"),
            "recovery should be surfaced as a diagnostic"
        );
        let _ = fs::remove_file(&state_path);
    }

    #[test]
    fn provider_does_not_retry_permanent_error() {
        let state_path = unique_state_path("permanent");
        let _ = fs::remove_file(&state_path);
        let command = format!("node {}", permanent_error_script(&state_path));
        let (request, provider) = request("task-permanent", command);

        let outcome = run_provider_command(&request, &provider);

        assert_eq!(outcome.status, AgentTaskOutcomeStatus::ProviderError);
        assert_eq!(
            outcome.failure_classification,
            Some(AgentTaskFailureClassification::Provider),
            "permanent auth/validation failures stay non-retryable"
        );
        let attempts: u32 = fs::read_to_string(&state_path)
            .ok()
            .and_then(|raw| raw.trim().parse().ok())
            .unwrap_or_default();
        assert_eq!(attempts, 1, "permanent error must fail fast, no retry");
        assert!(
            !outcome
                .diagnostics
                .iter()
                .any(|d| d.class == "agent_task.provider_transient_retry"),
            "permanent failures should not record retry history"
        );
        let _ = fs::remove_file(&state_path);
    }

    #[test]
    fn provider_exhausts_bounded_transient_retries() {
        let state_path = unique_state_path("exhaust");
        let _ = fs::remove_file(&state_path);
        // Always transient: never recovers within the bounded attempt budget.
        let command = format!("node {}", transient_then_success_script(&state_path, 999));
        let (request, provider) = request("task-transient-exhaust", command);

        let outcome = run_provider_command(&request, &provider);

        assert_eq!(
            outcome.status,
            AgentTaskOutcomeStatus::ProviderError,
            "persistent transient failure still fails after the bounded budget"
        );
        assert_eq!(
            outcome.failure_classification,
            Some(AgentTaskFailureClassification::Transient),
            "exhausted transient failures stay classified as transient/retryable"
        );
        let attempts: u32 = fs::read_to_string(&state_path)
            .ok()
            .and_then(|raw| raw.trim().parse().ok())
            .unwrap_or_default();
        assert_eq!(
            attempts, PROVIDER_TRANSIENT_MAX_ATTEMPTS,
            "retry budget is bounded to PROVIDER_TRANSIENT_MAX_ATTEMPTS"
        );
        assert!(
            outcome.diagnostics.iter().any(|d| {
                d.class == "agent_task.provider_transient_retry"
                    && d.data["retries_exhausted"] == json!(true)
            }),
            "exhaustion should be surfaced as a diagnostic"
        );
        let _ = fs::remove_file(&state_path);
    }
}
