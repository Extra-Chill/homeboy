use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub const AGENT_TASK_RUNTIME_TOOL_SCHEMA: &str = "homeboy/agent-task-runtime-tool/v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskRuntimeTool {
    #[serde(default = "runtime_tool_schema")]
    pub schema: String,
    pub id: String,
    /// The first supported transport is a local process connected over stdin/stdout.
    #[serde(default = "stdio_transport")]
    pub transport: String,
    /// Executable followed by its arguments. This deliberately avoids shell parsing.
    pub command: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub secret_env: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    #[serde(default)]
    pub readiness: AgentTaskRuntimeToolReadiness,
    #[serde(default)]
    pub lifecycle: AgentTaskRuntimeToolLifecycle,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AgentTaskRuntimeToolReadiness {
    /// Arguments after the executable that identify the tool for readiness
    /// probes. When omitted, probes retain all declared launch arguments.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_prefix: Option<Vec<String>>,
    /// Arguments used to collect a stable executable version before dispatch.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub version_command: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskRuntimeToolLifecycle {
    /// The runtime owns the child, which remains in the provider process group.
    #[default]
    RuntimeOwned,
}

#[cfg(test)]
impl AgentTaskRuntimeTool {
    pub(crate) fn redacted(mut self) -> Self {
        self.env = self
            .env
            .keys()
            .map(|name| (name.clone(), "[redacted]".to_string()))
            .collect();
        self
    }
}

fn runtime_tool_schema() -> String {
    AGENT_TASK_RUNTIME_TOOL_SCHEMA.to_string()
}

fn stdio_transport() -> String {
    "stdio".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_tool_contract_round_trips_and_redacts_literal_environment_values() {
        let tool: AgentTaskRuntimeTool = serde_json::from_value(serde_json::json!({
            "id": "fixture.browser",
            "command": ["fixture-mcp", "--isolated"],
            "env": { "FIXTURE_MODE": "private-value" },
            "secret_env": ["FIXTURE_TOKEN"],
            "timeout_ms": 1000,
            "readiness": { "version_command": ["--version"] }
        }))
        .expect("runtime tool declaration");

        assert_eq!(tool.schema, AGENT_TASK_RUNTIME_TOOL_SCHEMA);
        let redacted = serde_json::to_value(tool.redacted()).expect("redacted declaration");
        assert_eq!(redacted["env"]["FIXTURE_MODE"], "[redacted]");
        assert_eq!(redacted["secret_env"][0], "FIXTURE_TOKEN");
    }
}
