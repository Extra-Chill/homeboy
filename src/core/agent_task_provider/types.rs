use super::*;

pub const AGENT_TASK_EXECUTOR_PROVIDER_SCHEMA: &str = "homeboy/agent-task-executor-provider/v1";
pub const AGENT_TASK_PROVIDER_CAPABILITY_CONTRACT_SCHEMA: &str =
    "homeboy/agent-task-provider-capability-contract/v1";

/// Shared request/outcome schema identifiers carried by provider capability
/// contracts. Flattened into parents so the on-wire JSON keeps the
/// `request_schema` / `outcome_schema` keys inline.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderSchemaContract {
    pub request_schema: String,
    pub outcome_schema: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskProviderCapabilityContract {
    pub schema: String,
    pub provider_schema: String,
    #[serde(flatten)]
    pub schemas: ProviderSchemaContract,
    pub tool_request_schema: String,
    pub tool_result_schema: String,
    pub tool_policy_schema: String,
}

pub fn provider_capability_contract() -> AgentTaskProviderCapabilityContract {
    AgentTaskProviderCapabilityContract {
        schema: AGENT_TASK_PROVIDER_CAPABILITY_CONTRACT_SCHEMA.to_string(),
        provider_schema: AGENT_TASK_EXECUTOR_PROVIDER_SCHEMA.to_string(),
        schemas: ProviderSchemaContract {
            request_schema: AGENT_TASK_REQUEST_SCHEMA.to_string(),
            outcome_schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
        },
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
    #[serde(
        default,
        skip_serializing_if = "AgentTaskProviderResultContract::is_empty"
    )]
    pub result_contract: AgentTaskProviderResultContract,
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
pub struct AgentTaskProviderResultContract {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub typed_artifact_envelope: Option<AgentTaskProviderTypedArtifactEnvelopeContract>,
}

impl AgentTaskProviderResultContract {
    pub(super) fn is_empty(&self) -> bool {
        self.typed_artifact_envelope.is_none()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskProviderTypedArtifactEnvelopeContract {
    pub schema: String,
    #[serde(default = "default_provider_run_result_output")]
    pub output: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostic_class_prefix: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub private_shape_markers: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub require_typed_artifacts: Option<bool>,
}

impl Default for AgentTaskProviderTypedArtifactEnvelopeContract {
    fn default() -> Self {
        Self {
            schema: String::new(),
            output: default_provider_run_result_output(),
            provider_label: None,
            diagnostic_class_prefix: None,
            private_shape_markers: Vec::new(),
            require_typed_artifacts: None,
        }
    }
}

fn default_provider_run_result_output() -> String {
    "provider_run_result".to_string()
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

pub(super) fn default_metadata() -> Value {
    Value::Object(Default::default())
}

pub(super) fn is_empty_metadata(value: &Value) -> bool {
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
