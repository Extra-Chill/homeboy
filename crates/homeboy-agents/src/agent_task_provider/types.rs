use super::*;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentRuntimeDiscoveryDiagnostic {
    pub class: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extension_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

pub const AGENT_TASK_EXECUTOR_PROVIDER_SCHEMA: &str = "homeboy/agent-task-executor-provider/v1";
pub const DEFAULT_PI_READINESS_TIMEOUT_MS: u64 = 20_000;
const MAX_PI_READINESS_TIMEOUT_MS: u64 = 120_000;

fn validate_pi_readiness_timeout_ms(timeout_ms: u64) -> std::result::Result<(), String> {
    if (1..=MAX_PI_READINESS_TIMEOUT_MS).contains(&timeout_ms) {
        Ok(())
    } else {
        Err(format!(
            "pi_readiness.timeout_ms must be between 1 and {MAX_PI_READINESS_TIMEOUT_MS} milliseconds"
        ))
    }
}

pub const AGENT_TASK_PROVIDER_CAPABILITY_CONTRACT_SCHEMA: &str =
    "homeboy/agent-task-provider-capability-contract/v1";
/// Provider accepts `executor.config.workspace_permission_root` as the exact
/// task workspace path used for runtime permission policy.
pub const AGENT_TASK_PROVIDER_CAPABILITY_WORKSPACE_PERMISSION_ROOT_V1: &str =
    "workspace_permission_root/v1";

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
    #[serde(
        default,
        deserialize_with = "reject_deprecated_provider_command",
        skip_serializing_if = "String::is_empty"
    )]
    pub command: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub command_argv: Vec<String>,
    #[serde(default, skip_serializing_if = "CommandInvocation::is_empty")]
    pub invocation: CommandInvocation,
    #[serde(default = "default_request_schema")]
    pub request_schema: String,
    #[serde(default = "default_outcome_schema")]
    pub outcome_schema: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_materialization: Option<AgentTaskProviderWorkspaceMaterialization>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub provider_defaults: BTreeMap<String, Value>,
    #[serde(default)]
    pub pi_readiness: PiReadinessConfig,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extension_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extension_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_package_source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_path: Option<String>,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

/// Configuration for the built-in Pi readiness probe.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PiReadinessConfig {
    #[serde(default)]
    pub env_allowlist: Vec<String>,
    #[serde(
        default = "default_pi_readiness_timeout_ms",
        deserialize_with = "deserialize_pi_readiness_timeout_ms"
    )]
    pub timeout_ms: u64,
}

impl Default for PiReadinessConfig {
    fn default() -> Self {
        Self {
            env_allowlist: Vec::new(),
            timeout_ms: DEFAULT_PI_READINESS_TIMEOUT_MS,
        }
    }
}

fn default_pi_readiness_timeout_ms() -> u64 {
    DEFAULT_PI_READINESS_TIMEOUT_MS
}

fn deserialize_pi_readiness_timeout_ms<'de, D>(
    deserializer: D,
) -> std::result::Result<u64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let timeout_ms = u64::deserialize(deserializer)?;
    validate_pi_readiness_timeout_ms(timeout_ms).map_err(serde::de::Error::custom)?;
    Ok(timeout_ms)
}

fn reject_deprecated_provider_command<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let command = Option::<String>::deserialize(deserializer)?.unwrap_or_default();
    if command.trim().is_empty() {
        return Ok(command);
    }

    Err(<D::Error as serde::de::Error>::custom(
        "agent-task provider string-form 'command' is no longer supported; use invocation.argv or argv instead",
    ))
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

pub(super) fn default_metadata() -> Value {
    Value::Object(Default::default())
}

pub(super) fn is_empty_metadata(value: &Value) -> bool {
    value.as_object().is_none_or(|object| object.is_empty())
}
