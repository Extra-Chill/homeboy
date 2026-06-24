use super::*;

pub const AGENT_TASK_EXECUTOR_PROVIDER_SCHEMA: &str = "homeboy/agent-task-executor-provider/v1";
pub const AGENT_TASK_PROVIDER_CAPABILITY_CONTRACT_SCHEMA: &str =
    "homeboy/agent-task-provider-capability-contract/v1";

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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lab_runtime_components: Vec<String>,
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
    /// Runtime dependency reconciliation contract: packages the runtime/overlay
    /// (e.g. "WordPress 7.0 supplies this Composer package") provides, which a
    /// staged provider plugin must NOT vendor/shadow. Homeboy validates the
    /// effective staged plugin against this before dispatch so a stale vendored
    /// runtime library fails with an actionable owner/contract message instead of
    /// a raw PHP fatal during plugin activation (#6223).
    #[serde(
        default,
        skip_serializing_if = "AgentTaskRuntimeStagingContract::is_empty"
    )]
    pub staging: AgentTaskRuntimeStagingContract,
}

impl AgentTaskRuntimeContract {
    fn is_empty(&self) -> bool {
        self.capabilities.is_empty()
            && self.lifecycle_states.is_empty()
            && self.normalization.is_empty()
            && self.apply_back.is_empty()
            && self.staging.is_empty()
    }
}

/// Declares the runtime dependency reconciliation surface for a provider's
/// staged plugins. Core is runtime-agnostic: it does not know that a package is
/// a Composer dependency or that the owner is WordPress — the declaring
/// extension supplies the package name, the owner that provides it at runtime,
/// and the vendor subpaths a staged plugin would use to shadow it. Homeboy uses
/// this to reconcile staged plugins before dispatch (#6223).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AgentTaskRuntimeStagingContract {
    /// Packages the runtime/overlay owns. A staged provider plugin that vendors
    /// any of these would shadow the runtime-provided version and is refused
    /// before dispatch with an actionable owner/package/contract error.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reconciled_packages: Vec<AgentTaskRuntimeReconciledPackage>,
    /// Optional human-facing remediation appended to reconciliation conflict
    /// errors (e.g. how to rebuild the staged plugin without the vendored copy).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remediation: Option<String>,
    /// When true (default), Homeboy validates staged plugins against this
    /// contract before dispatch. Set false to declare the contract for evidence
    /// without enforcing the pre-dispatch gate (e.g. when WP Codebox owns the
    /// authoritative readiness check and returns a structured result).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub validate_before_dispatch: Option<bool>,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

impl AgentTaskRuntimeStagingContract {
    pub fn is_empty(&self) -> bool {
        self.reconciled_packages.is_empty()
            && self.remediation.is_none()
            && self.validate_before_dispatch.is_none()
            && self.extra.is_empty()
    }

    /// Whether the pre-dispatch validation gate is enforced. Defaults to true
    /// when any reconciled package is declared, so declaring a package is enough
    /// to opt into the gate; an explicit `validate_before_dispatch: false`
    /// records the contract for evidence/Codebox delegation without gating.
    pub fn enforces_pre_dispatch(&self) -> bool {
        self.validate_before_dispatch.unwrap_or(true) && !self.reconciled_packages.is_empty()
    }
}

/// A single package the runtime owns that staged provider plugins must not
/// vendor. Generic by design: `name` is the package identity, `owner` is the
/// runtime/overlay that supplies it (named in conflict errors), and
/// `vendor_subpaths` are the staged-plugin-relative paths whose presence proves
/// the staged plugin shadows the runtime copy (e.g. `vendor/<org>/<pkg>`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AgentTaskRuntimeReconciledPackage {
    /// Package identity (e.g. a Composer package name `org/library`).
    pub name: String,
    /// The runtime/overlay that supplies this package at runtime. Surfaced in
    /// conflict errors so the operator knows who owns the canonical copy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    /// Staged-plugin-relative paths that, if present in the effective staged
    /// plugin, prove it vendors (and would shadow) the runtime-owned package.
    /// When omitted, `vendor/<name>` is checked by default.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub vendor_subpaths: Vec<String>,
    /// Optional reason/why this package is reconciled, surfaced in diagnostics.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Optional per-package remediation, surfaced when this package conflicts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remediation: Option<String>,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

impl AgentTaskRuntimeReconciledPackage {
    /// The staged-plugin-relative subpaths whose presence indicates a shadowed
    /// runtime package. Falls back to `vendor/<name>` when none are declared.
    pub fn effective_vendor_subpaths(&self) -> Vec<String> {
        let declared: Vec<String> = self
            .vendor_subpaths
            .iter()
            .map(|subpath| subpath.trim().trim_matches('/').to_string())
            .filter(|subpath| !subpath.is_empty())
            .collect();
        if !declared.is_empty() {
            return declared;
        }
        let name = self.name.trim().trim_matches('/');
        if name.is_empty() {
            return Vec::new();
        }
        vec![format!("vendor/{name}")]
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
pub(super) struct AgentTaskProviderResolvedExecutable {
    pub(super) env: Vec<String>,
    pub(super) path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct AgentTaskProviderExecutableResolutionError {
    pub(super) readiness_id: String,
    pub(super) label: String,
    pub(super) env: Vec<String>,
    pub(super) candidates: Vec<String>,
    pub(super) install_hint: Option<String>,
}

impl AgentTaskProviderExecutableResolutionError {
    pub(super) fn message(&self) -> String {
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

    pub(super) fn role_for_artifact_kind(&self, kind: &str) -> Option<&str> {
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
