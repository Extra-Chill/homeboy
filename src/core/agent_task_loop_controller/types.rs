use super::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

pub const AGENT_TASK_LOOP_CONTROLLER_SCHEMA: &str = "homeboy/agent-task-loop-controller/v1";
pub const AGENT_TASK_LOOP_CONTROLLER_STATUS_SCHEMA: &str =
    "homeboy/agent-task-loop-controller-status/v1";
pub(crate) const STALE_PENDING_ACTION_SECONDS: i64 = 24 * 60 * 60;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskLoopControllerRecord {
    #[serde(default = "controller_schema")]
    pub schema: String,
    pub loop_id: String,
    pub phase: String,
    pub state: AgentTaskLoopControllerState,
    pub config_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_loop_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_action_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_entity_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub entities: BTreeMap<String, AgentTaskLoopEntity>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub dedupe_keys: BTreeMap<String, AgentTaskLoopDedupeRecord>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub task_lineage: Vec<AgentTaskLoopTaskLineage>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gate_bundles: Vec<AgentTaskGateBundle>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gate_results: Vec<AgentTaskGateBundleResult>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub terminal_outcomes: Vec<AgentTaskLoopTerminalOutcome>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub waits: Vec<AgentTaskLoopWait>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subcontrollers: Vec<AgentTaskLoopSubcontrollerRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub feedback: Vec<AgentTaskLoopFeedbackArtifact>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pr_ownerships: Vec<AgentTaskPrOwnershipRecord>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub next_actions: Vec<AgentTaskLoopPolicyActionRecord>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub history: Vec<AgentTaskLoopHistoryEvent>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskLoopControllerState {
    Running,
    Waiting,
    HumanReady,
    Completed,
    Abandoned,
    Escalated,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskLoopEntity {
    pub entity_id: String,
    pub entity_type: String,
    pub key: String,
    pub dedupe_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    #[serde(default)]
    pub human_ready: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parent_entity_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub run_refs: Vec<AgentTaskLoopRunRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_refs: Vec<AgentTaskLoopArtifactRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub provenance: Vec<AgentTaskLoopProvenanceRef>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskLoopRunRef {
    pub run_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskLoopArtifactRef {
    pub uri: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_key: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskLoopProvenanceRef {
    pub kind: String,
    pub uri: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caused_by: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskLoopDedupeRecord {
    pub dedupe_key: String,
    pub action: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_ref: Option<String>,
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskLoopSubcontrollerRef {
    pub loop_id: String,
    pub dedupe_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_loop_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_action_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait_key: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub terminal_states: Vec<AgentTaskLoopControllerState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<AgentTaskLoopControllerState>,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub request: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskLoopTaskLineage {
    pub run_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dedupe_key: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_refs: Vec<AgentTaskLoopArtifactRef>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub inputs: Value,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub outputs: Value,
}
