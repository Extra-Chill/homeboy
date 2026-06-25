use super::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskGateBundle {
    pub bundle_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checks: Vec<AgentTaskGateBundleCheck>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskGateBundleCheck {
    pub check_id: String,
    pub kind: AgentTaskGateBundleCheckKind,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub input: Value,
    #[serde(default)]
    pub retryable: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskGateBundleCheckKind {
    Command,
    Api,
    Tool,
    Manual,
}

impl AgentTaskGateBundle {
    // Part of the loop-controller API exercised only by tests; production wiring is pending.
    #[cfg(test)]
    pub(crate) fn from_verify_commands(
        bundle_id: impl Into<String>,
        commands: Vec<String>,
    ) -> Self {
        Self {
            bundle_id: bundle_id.into(),
            description: "legacy --verify command gate bundle".to_string(),
            checks: commands
                .into_iter()
                .enumerate()
                .map(|(index, command)| AgentTaskGateBundleCheck {
                    check_id: format!("verify-{}", index + 1),
                    kind: AgentTaskGateBundleCheckKind::Command,
                    input: json!({ "command": command }),
                    retryable: true,
                })
                .collect(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskGateBundleResult {
    pub result_id: String,
    pub bundle_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    pub status: AgentTaskGateBundleStatus,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checks: Vec<AgentTaskGateCheckResult>,
    pub recorded_at: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskGateBundleStatus {
    Passed,
    Failed,
    Warn,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskLoopTerminalStatus {
    Passed,
    BlockedByGate,
    NoPublication,
    NoActionableFindings,
    NeedsRevalidation,
    NeedsUpstreamFix,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskLoopTerminalOutcome {
    pub outcome_id: String,
    pub status: AgentTaskLoopTerminalStatus,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity_id: Option<String>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub details: Value,
    pub recorded_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskGateCheckResult {
    pub check_id: String,
    pub status: AgentTaskGateBundleStatus,
    #[serde(default)]
    pub retryable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub classification: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<AgentTaskLoopArtifactRef>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub details: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskLoopWait {
    pub wait_key: String,
    pub event_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub escalation_policy: Option<String>,
    #[serde(default = "open_wait_status")]
    pub status: AgentTaskLoopWaitStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub satisfied_by_event_id: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskLoopWaitStatus {
    Open,
    Satisfied,
    Cancelled,
    TimedOut,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskLoopFeedbackArtifact {
    pub feedback_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_entity_id: Option<String>,
    pub status: AgentTaskLoopFeedbackStatus,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub findings: Vec<AgentTaskLoopReviewFinding>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub payload: Value,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskLoopFeedbackStatus {
    Informational,
    ChangesRequested,
    Approved,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskLoopReviewFinding {
    pub finding_id: String,
    pub severity: String,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dedupe_key: Option<String>,
}
