use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

use super::schema::{
    agent_tool_policy_schema, agent_tool_request_schema, agent_tool_result_schema,
    default_agent_tool_execution_location, default_apply_policy, default_read_policy,
    default_write_policy, AGENT_TOOL_POLICY_SCHEMA,
};
use super::AgentTaskDiagnostic;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskPolicy {
    #[serde(default = "default_read_policy")]
    pub read: String,
    #[serde(default = "default_write_policy")]
    pub write: String,
    #[serde(default = "default_apply_policy")]
    pub apply: String,
    #[serde(
        default,
        alias = "toolPolicy",
        skip_serializing_if = "AgentToolPolicy::is_default"
    )]
    pub tools: AgentToolPolicy,
}

impl Default for AgentTaskPolicy {
    fn default() -> Self {
        Self {
            read: default_read_policy(),
            write: default_write_policy(),
            apply: default_apply_policy(),
            tools: AgentToolPolicy::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentToolRequest {
    #[serde(default = "agent_tool_request_schema")]
    pub schema: String,
    pub request_id: String,
    pub task_id: String,
    pub tool: String,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub input: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
}

impl AgentToolRequest {
    pub fn redacted(&self) -> Self {
        let policy = crate::core::redaction::RedactionPolicy::default();
        let mut redacted = self.clone();
        redacted.input = policy.redact_json(&redacted.input);
        redacted.metadata = policy.redact_json(&redacted.metadata);
        redacted
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentToolResult {
    #[serde(default = "agent_tool_result_schema")]
    pub schema: String,
    pub request_id: String,
    pub task_id: String,
    pub tool: String,
    pub status: AgentToolResultStatus,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub output: Value,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<AgentTaskDiagnostic>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
}

impl AgentToolResult {
    pub fn redacted(&self) -> Self {
        let policy = crate::core::redaction::RedactionPolicy::default();
        let mut redacted = self.clone();
        redacted.output = policy.redact_json(&redacted.output);
        redacted.diagnostics = redacted
            .diagnostics
            .into_iter()
            .map(|diagnostic| diagnostic.redacted_with(&policy))
            .collect();
        redacted.metadata = policy.redact_json(&redacted.metadata);
        redacted
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentToolResultStatus {
    Succeeded,
    Failed,
    Denied,
    Timeout,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentToolPolicy {
    #[serde(default = "agent_tool_policy_schema")]
    pub schema: String,
    #[serde(default = "default_agent_tool_execution_location")]
    pub default_location: AgentToolExecutionLocation,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tools: BTreeMap<String, AgentToolPolicyRule>,
}

impl AgentToolPolicy {
    pub fn execution_location_for(&self, tool: &str) -> AgentToolExecutionLocation {
        self.tools
            .get(tool)
            .map(|rule| rule.execution_location)
            .unwrap_or(self.default_location)
    }

    fn is_default(&self) -> bool {
        self == &Self::default()
    }
}

impl Default for AgentToolPolicy {
    fn default() -> Self {
        Self {
            schema: AGENT_TOOL_POLICY_SCHEMA.to_string(),
            default_location: default_agent_tool_execution_location(),
            tools: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentToolPolicyRule {
    pub execution_location: AgentToolExecutionLocation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentToolExecutionLocation {
    Runner,
    ControlPlane,
    Disabled,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskLimits {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_runtime_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_bytes: Option<u64>,
}
