use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

use super::command_policy::{AgentCommandDecision, AgentCommandPolicy};
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

impl AgentTaskPolicy {
    /// Permit a remediation provider to inspect its task workspace through its
    /// runner-owned read tool without granting any write tool.
    pub(crate) fn grant_workspace_read_tool(&mut self) {
        self.read = "workspace".to_string();
        self.tools.tools.insert(
            "read".to_string(),
            AgentToolPolicyRule {
                execution_location: AgentToolExecutionLocation::Runner,
                timeout_ms: None,
                reason: Some("inspect the task workspace during remediation".to_string()),
            },
        );
    }

    pub(crate) fn permits_workspace_read_tool(&self) -> bool {
        self.read == "workspace"
            && self.tools.execution_location_for("read") == AgentToolExecutionLocation::Runner
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
        let policy = homeboy_core::redaction::RedactionPolicy::default();
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
        let policy = homeboy_core::redaction::RedactionPolicy::default();
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
    /// Which *commands* the agent may run, independent of which tool runs them.
    /// The per-tool rules above choose an execution location for a tool name;
    /// this constrains the command a shell-shaped tool is about to execute
    /// (#11481).
    #[serde(
        default,
        alias = "commandPolicy",
        skip_serializing_if = "AgentCommandPolicy::is_default"
    )]
    pub commands: AgentCommandPolicy,
}

impl AgentToolPolicy {
    pub fn execution_location_for(&self, tool: &str) -> AgentToolExecutionLocation {
        self.tools
            .get(tool)
            .map(|rule| rule.execution_location)
            .unwrap_or(self.default_location)
    }

    /// Decide whether a command line the agent wants to run is permitted.
    pub fn evaluate_command(&self, command: &str) -> AgentCommandDecision {
        self.commands.evaluate(command)
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
            commands: AgentCommandPolicy::default(),
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
    /// Absolute UTC Unix timestamp inherited from the plan execution budget.
    /// Providers and remote runners use its remaining time rather than starting
    /// another lifecycle-local timeout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_deadline_unix_ms: Option<u64>,
    /// Per-attempt liveness deadline: if the provider produces no
    /// stdout/stderr progress within this window, the attempt is killed and
    /// classified as stalled/rate_limited so rotation can advance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub liveness_timeout_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_bytes: Option<u64>,
    /// Stable, non-secret identifiers for exclusive host resources required by
    /// this attempt. The scheduler acquires these only when it dispatches the
    /// executor, so queue and resource wait do not consume `timeout_ms`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclusive_resource_keys: Vec<String>,
}
