use super::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskLoopExternalEvent {
    pub event_id: String,
    pub event_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity_id: Option<String>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub payload: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskLoopHistoryEvent {
    pub event_id: String,
    pub event_type: String,
    pub recorded_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity_id: Option<String>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub payload: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskLoopControllerStatusReport {
    pub schema: String,
    pub controller: AgentTaskLoopControllerRecord,
    pub diagnostics: AgentTaskLoopControllerDiagnostics,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskLoopControllerDiagnostics {
    pub schema: String,
    pub stale_pending_threshold_seconds: i64,
    pub summary: AgentTaskLoopControllerDiagnosticSummary,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failed_child_actions: Vec<AgentTaskLoopFailedChildActionDiagnostic>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_actions: Vec<AgentTaskLoopPendingActionDiagnostic>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub acceptance_gates: Vec<AgentTaskLoopAcceptanceGateDiagnostic>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskLoopControllerDiagnosticSummary {
    pub pending_action_count: usize,
    pub failed_child_action_count: usize,
    pub stale_pending_action_count: usize,
    pub orphaned_pending_action_count: usize,
    pub acceptance_gate_count: usize,
    pub missing_acceptance_gate_count: usize,
    pub failed_acceptance_gate_count: usize,
    #[serde(default)]
    pub pending_acceptance_gate_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskLoopFailedChildActionDiagnostic {
    pub action_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dedupe_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_run_status: Option<String>,
    pub top_diagnostic: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hydrated_root_cause: Option<String>,
    pub owner_surface: String,
    pub next_command: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_refs: Vec<AgentTaskLoopFailedChildEvidenceRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskLoopFailedChildEvidenceRef {
    pub kind: String,
    pub uri: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskLoopAcceptanceGateStatus {
    Satisfied,
    Missing,
    Failed,
    Warning,
    /// The gate recorded a result that is still awaiting an external/manual
    /// signal. Treated as blocking (not acceptable) so a manual-only bundle
    /// never resolves to a false-green acceptance gate.
    Pending,
}

/// Canonical projection from a (possibly absent) recorded gate-bundle result
/// status to the acceptance-gate status surfaced in diagnostics. An absent
/// result maps to `Missing`; the present statuses map 1:1 onto their
/// acceptance-gate equivalents. Routing every call site through this `From`
/// keeps the projection in one place instead of hand-synced match arms.
impl From<Option<AgentTaskGateBundleStatus>> for AgentTaskLoopAcceptanceGateStatus {
    fn from(status: Option<AgentTaskGateBundleStatus>) -> Self {
        match status {
            Some(AgentTaskGateBundleStatus::Passed) => AgentTaskLoopAcceptanceGateStatus::Satisfied,
            Some(AgentTaskGateBundleStatus::Failed) => AgentTaskLoopAcceptanceGateStatus::Failed,
            Some(AgentTaskGateBundleStatus::Warn) => AgentTaskLoopAcceptanceGateStatus::Warning,
            Some(AgentTaskGateBundleStatus::Pending) => AgentTaskLoopAcceptanceGateStatus::Pending,
            None => AgentTaskLoopAcceptanceGateStatus::Missing,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskLoopAcceptanceGateDiagnostic {
    pub bundle_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity_id: Option<String>,
    pub status: AgentTaskLoopAcceptanceGateStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_status: Option<AgentTaskGateBundleStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recorded_at: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub problems: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskLoopPendingActionDiagnostic {
    pub action_id: String,
    pub action: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dedupe_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runner_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub referenced_run_id: Option<String>,
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub age_seconds: Option<i64>,
    pub stale: bool,
    pub orphaned: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub problems: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recovery_commands: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acceptance_gate_status_bridges_from_bundle_status() {
        assert_eq!(
            AgentTaskLoopAcceptanceGateStatus::from(Some(AgentTaskGateBundleStatus::Passed)),
            AgentTaskLoopAcceptanceGateStatus::Satisfied
        );
        assert_eq!(
            AgentTaskLoopAcceptanceGateStatus::from(Some(AgentTaskGateBundleStatus::Failed)),
            AgentTaskLoopAcceptanceGateStatus::Failed
        );
        assert_eq!(
            AgentTaskLoopAcceptanceGateStatus::from(Some(AgentTaskGateBundleStatus::Warn)),
            AgentTaskLoopAcceptanceGateStatus::Warning
        );
        assert_eq!(
            AgentTaskLoopAcceptanceGateStatus::from(Some(AgentTaskGateBundleStatus::Pending)),
            AgentTaskLoopAcceptanceGateStatus::Pending
        );
        assert_eq!(
            AgentTaskLoopAcceptanceGateStatus::from(None),
            AgentTaskLoopAcceptanceGateStatus::Missing
        );
    }
}
