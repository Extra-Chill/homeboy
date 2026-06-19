use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs;
use std::io::ErrorKind;
use std::path::PathBuf;

use crate::core::agent_task_lifecycle::AgentTaskRunState;
use crate::core::agent_task_loop_runner_policy::{
    blocked_runner_decision, runner_policy_for_action,
};
pub use crate::core::agent_task_loop_runner_policy::{
    AgentTaskLoopLocalFallbackPolicy, AgentTaskLoopRunnerAvailability,
    AgentTaskLoopRunnerExecutionTarget, AgentTaskLoopRunnerPolicy,
    AgentTaskLoopRunnerPolicyDecision,
};
use crate::core::{agent_task_lifecycle, paths, Error, Result};

pub const AGENT_TASK_LOOP_CONTROLLER_SCHEMA: &str = "homeboy/agent-task-loop-controller/v1";
pub const AGENT_TASK_LOOP_CONTROLLER_STATUS_SCHEMA: &str =
    "homeboy/agent-task-loop-controller-status/v1";
const STALE_PENDING_ACTION_SECONDS: i64 = 24 * 60 * 60;

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
    pub label: Option<String>,
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskLoopPolicy {
    pub policy_id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub transitions: Vec<AgentTaskLoopTransition>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskLoopTransition {
    pub transition_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_phase: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_event_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when_json_path: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<AgentTaskLoopPolicyAction>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum AgentTaskLoopPolicyAction {
    SpawnTask {
        dedupe_key: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        entity_id: Option<String>,
        #[serde(default, skip_serializing_if = "Value::is_null")]
        request: Value,
    },
    FanOut {
        dedupe_key: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        entity_ids: Vec<String>,
        #[serde(default, skip_serializing_if = "Value::is_null")]
        request_template: Value,
    },
    SpawnController {
        dedupe_key: String,
        loop_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        entity_id: Option<String>,
        #[serde(default = "default_controller_phase")]
        phase: String,
        #[serde(default = "default_config_version")]
        config_version: String,
        #[serde(default, skip_serializing_if = "Value::is_null")]
        request: Value,
    },
    SpawnSubloop {
        dedupe_key: String,
        loop_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        entity_id: Option<String>,
        #[serde(default = "default_controller_phase")]
        phase: String,
        #[serde(default = "default_config_version")]
        config_version: String,
        #[serde(default, skip_serializing_if = "Value::is_null")]
        request: Value,
    },
    RouteFinding {
        finding: AgentTaskLoopFindingPacket,
        dedupe_key: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        entity_id: Option<String>,
        #[serde(default, skip_serializing_if = "Value::is_null")]
        request_template: Value,
    },
    ValidateCandidatePatch {
        candidate: AgentTaskLoopCandidatePatch,
        validation: AgentTaskLoopCandidateValidation,
        #[serde(default)]
        limits: AgentTaskLoopCandidateLoopLimits,
    },
    Join {
        wait_key: String,
    },
    Retry {
        target_run_id: String,
    },
    RequestChanges {
        target_run_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        feedback_id: Option<String>,
    },
    RunGates {
        bundle_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        entity_id: Option<String>,
    },
    OwnPrUntilGreen {
        ownership: AgentTaskPrOwnershipRequest,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        entity_id: Option<String>,
    },
    WaitForEvent(AgentTaskLoopWait),
    WaitForController {
        loop_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        entity_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        wait_key: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        terminal_states: Vec<AgentTaskLoopControllerState>,
    },
    MarkHumanReady {
        entity_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    Complete {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    Abandon {
        reason: String,
    },
    Escalate {
        reason: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskLoopPolicyActionRecord {
    pub action_id: String,
    pub action: AgentTaskLoopPolicyAction,
    pub status: AgentTaskLoopActionStatus,
    pub reason: String,
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dedupe_key: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<AgentTaskLoopActionDiagnostic>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskLoopActionDiagnostic {
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runner: Option<String>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub details: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskLoopFindingPacket {
    pub finding_id: String,
    pub severity: String,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_transformer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reproduction_key: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lineage: Vec<AgentTaskLoopArtifactRef>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub payload: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskLoopCandidatePatch {
    pub candidate_id: String,
    pub patch: AgentTaskLoopArtifactRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finding_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<String>,
    #[serde(default)]
    pub attempt: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lineage: Vec<AgentTaskLoopArtifactRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskLoopCandidateValidation {
    pub validation_id: String,
    pub status: AgentTaskLoopCandidateValidationStatus,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<AgentTaskLoopArtifactRef>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub details: Value,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskLoopCandidateValidationStatus {
    Passed,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskLoopCandidateLoopLimits {
    #[serde(default = "default_candidate_max_attempts")]
    pub max_attempts: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskPrOwnershipRequest {
    pub ownership_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub component_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub base: String,
    pub head: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr_number: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr_url: Option<String>,
    #[serde(default = "default_pr_ownership_max_retries")]
    pub max_retries: u32,
    #[serde(default)]
    pub merge_required: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskPrOwnershipRecord {
    pub ownership_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity_id: Option<String>,
    pub state: AgentTaskPrOwnershipState,
    pub base: String,
    pub head: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr_number: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_sha: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ci_state: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ci_summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_decision: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merge_state: Option<String>,
    #[serde(default)]
    pub retry_count: u32,
    #[serde(default = "default_pr_ownership_max_retries")]
    pub max_retries: u32,
    #[serde(default)]
    pub merge_required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_checked_at: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<AgentTaskLoopArtifactRef>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskPrOwnershipState {
    Tracking,
    WaitingForChecks,
    ChangesRequested,
    RetryLimitReached,
    GreenReady,
    WaitingForMerge,
    Merged,
    MissingPr,
    Stopped,
}

impl Default for AgentTaskLoopCandidateLoopLimits {
    fn default() -> Self {
        Self {
            max_attempts: default_candidate_max_attempts(),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskLoopActionStatus {
    Pending,
    Running,
    AlreadySatisfied,
    Completed,
    Failed,
    BlockedRunnerUnavailable,
    BlockedRemoteMaterialization,
    BlockedLocalFallbackDenied,
}

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

#[derive(Debug, Clone, Default, PartialEq)]
pub struct AgentTaskPrOwnershipStatusUpdate {
    pub pr_number: Option<u64>,
    pub pr_url: Option<String>,
    pub head_sha: Option<String>,
    pub ci_state: Option<String>,
    pub ci_summary: Option<String>,
    pub review_decision: Option<String>,
    pub merge_state: Option<String>,
    pub retry_count: u32,
    pub evidence: Vec<AgentTaskLoopArtifactRef>,
    pub missing_pr: bool,
}

impl AgentTaskPrOwnershipStatusUpdate {
    pub fn tracking() -> Self {
        Self {
            ci_state: Some("tracking".to_string()),
            ..Self::default()
        }
    }
}

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
    pub pending_actions: Vec<AgentTaskLoopPendingActionDiagnostic>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskLoopControllerDiagnosticSummary {
    pub pending_action_count: usize,
    pub stale_pending_action_count: usize,
    pub orphaned_pending_action_count: usize,
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

impl AgentTaskLoopControllerRecord {
    pub fn new(
        loop_id: impl Into<String>,
        phase: impl Into<String>,
        config_version: impl Into<String>,
    ) -> Self {
        let now = now_timestamp();
        Self {
            schema: AGENT_TASK_LOOP_CONTROLLER_SCHEMA.to_string(),
            loop_id: sanitize_loop_id(&loop_id.into()),
            phase: phase.into(),
            state: AgentTaskLoopControllerState::Running,
            config_version: config_version.into(),
            parent_loop_id: None,
            parent_action_id: None,
            parent_entity_id: None,
            created_at: now.clone(),
            updated_at: now,
            entities: BTreeMap::new(),
            dedupe_keys: BTreeMap::new(),
            task_lineage: Vec::new(),
            gate_bundles: Vec::new(),
            gate_results: Vec::new(),
            terminal_outcomes: Vec::new(),
            waits: Vec::new(),
            subcontrollers: Vec::new(),
            feedback: Vec::new(),
            pr_ownerships: Vec::new(),
            next_actions: Vec::new(),
            history: Vec::new(),
            metadata: Value::Null,
        }
    }

    pub fn upsert_entity(
        &mut self,
        entity_type: impl Into<String>,
        key: impl Into<String>,
        parent_entity_ids: Vec<String>,
        metadata: Value,
    ) -> String {
        let entity_type = entity_type.into();
        let key = key.into();
        let dedupe_key = entity_dedupe_key(&entity_type, &key);
        if let Some(existing) = self.dedupe_keys.get(&dedupe_key) {
            if let Some(entity_id) = &existing.entity_id {
                return entity_id.clone();
            }
        }

        let entity_id = format!("{}:{}", entity_type, sanitize_loop_id(&key));
        let entity = AgentTaskLoopEntity {
            entity_id: entity_id.clone(),
            entity_type,
            key,
            dedupe_key: dedupe_key.clone(),
            state: None,
            human_ready: false,
            parent_entity_ids,
            run_refs: Vec::new(),
            artifact_refs: Vec::new(),
            provenance: Vec::new(),
            metadata,
        };
        self.entities.insert(entity_id.clone(), entity);
        self.dedupe_keys.insert(
            dedupe_key.clone(),
            AgentTaskLoopDedupeRecord {
                dedupe_key,
                action: "entity".to_string(),
                entity_id: Some(entity_id.clone()),
                run_id: None,
                external_ref: None,
                created_at: now_timestamp(),
                reason: Some("entity key registered".to_string()),
            },
        );
        self.touch();
        entity_id
    }

    pub fn apply_event(
        &mut self,
        event: AgentTaskLoopExternalEvent,
    ) -> Vec<AgentTaskLoopPolicyActionRecord> {
        let recorded_at = now_timestamp();
        self.history.push(AgentTaskLoopHistoryEvent {
            event_id: event.event_id.clone(),
            event_type: event.event_type.clone(),
            recorded_at,
            entity_id: event.entity_id.clone(),
            payload: event.payload.clone(),
        });

        for wait in &mut self.waits {
            if wait.status != AgentTaskLoopWaitStatus::Open || wait.event_type != event.event_type {
                continue;
            }
            let entity_matches = wait.entity_id.is_none() || wait.entity_id == event.entity_id;
            let external_matches =
                wait.external_ref.is_none() || wait.external_ref == event.event_key;
            if entity_matches && external_matches {
                wait.status = AgentTaskLoopWaitStatus::Satisfied;
                wait.satisfied_by_event_id = Some(event.event_id.clone());
            }
        }

        if self.open_wait_count() == 0 && self.state == AgentTaskLoopControllerState::Waiting {
            self.state = AgentTaskLoopControllerState::Running;
        }

        let mut actions = Vec::new();
        if let Some(policy) = event
            .payload
            .get("policy")
            .and_then(|value| serde_json::from_value::<AgentTaskLoopPolicy>(value.clone()).ok())
        {
            actions = self.evaluate_policy(&policy, Some(&event));
        }
        self.touch();
        actions
    }

    pub fn evaluate_policy(
        &mut self,
        policy: &AgentTaskLoopPolicy,
        event: Option<&AgentTaskLoopExternalEvent>,
    ) -> Vec<AgentTaskLoopPolicyActionRecord> {
        let mut records = Vec::new();
        for transition in &policy.transitions {
            if !self.transition_matches(transition, event) {
                continue;
            }
            for action in &transition.actions {
                records.push(self.record_action(
                    action.clone(),
                    format!(
                        "policy {} transition {} matched",
                        policy.policy_id, transition.transition_id
                    ),
                ));
            }
        }
        self.touch();
        records
    }

    fn transition_matches(
        &self,
        transition: &AgentTaskLoopTransition,
        event: Option<&AgentTaskLoopExternalEvent>,
    ) -> bool {
        if transition
            .from_phase
            .as_deref()
            .is_some_and(|phase| phase != self.phase)
        {
            return false;
        }
        if let Some(expected_event_type) = &transition.on_event_type {
            if event.map(|event| event.event_type.as_str()) != Some(expected_event_type.as_str()) {
                return false;
            }
        }
        let Some(expr) = &transition.when_json_path else {
            return true;
        };
        let Ok(path) = serde_json_path::JsonPath::parse(expr) else {
            return false;
        };
        let context = json!({
            "controller": self,
            "event": event,
        });
        path.query(&context)
            .all()
            .into_iter()
            .any(jsonpath_match_is_truthy)
    }

    pub fn record_action(
        &mut self,
        action: AgentTaskLoopPolicyAction,
        reason: impl Into<String>,
    ) -> AgentTaskLoopPolicyActionRecord {
        let reason = reason.into();
        let dedupe_key = action_dedupe_key(&action);
        let status = if let Some(dedupe_key) = &dedupe_key {
            if self.dedupe_keys.contains_key(dedupe_key) {
                AgentTaskLoopActionStatus::AlreadySatisfied
            } else {
                self.dedupe_keys.insert(
                    dedupe_key.clone(),
                    AgentTaskLoopDedupeRecord {
                        dedupe_key: dedupe_key.clone(),
                        action: action_name(&action).to_string(),
                        entity_id: action_entity_id(&action),
                        run_id: None,
                        external_ref: None,
                        created_at: now_timestamp(),
                        reason: Some(reason.clone()),
                    },
                );
                AgentTaskLoopActionStatus::Pending
            }
        } else {
            AgentTaskLoopActionStatus::Pending
        };

        let action_id = format!("action-{}", self.next_actions.len() + 1);
        self.apply_action_side_effects(&action, status, &action_id);
        let record = AgentTaskLoopPolicyActionRecord {
            action_id,
            action,
            status,
            reason,
            created_at: now_timestamp(),
            dedupe_key,
            diagnostics: Vec::new(),
        };
        self.next_actions.push(record.clone());
        self.touch();
        record
    }

    // Part of the loop-controller API exercised only by tests; production wiring is pending.
    #[cfg(test)]
    pub(crate) fn block_action_for_runner_policy(
        &mut self,
        action_id: &str,
        status: AgentTaskLoopActionStatus,
        diagnostic: AgentTaskLoopActionDiagnostic,
    ) -> Result<()> {
        if !matches!(
            status,
            AgentTaskLoopActionStatus::BlockedRunnerUnavailable
                | AgentTaskLoopActionStatus::BlockedRemoteMaterialization
                | AgentTaskLoopActionStatus::BlockedLocalFallbackDenied
        ) {
            return Err(Error::validation_invalid_argument(
                "status",
                "runner policy blocks must use a blocked action status",
                Some(format!("{status:?}")),
                None,
            ));
        }

        let action = self
            .next_actions
            .iter_mut()
            .find(|action| action.action_id == action_id)
            .ok_or_else(|| {
                Error::validation_invalid_argument(
                    "action_id",
                    format!("loop action '{action_id}' does not exist"),
                    Some(action_id.to_string()),
                    None,
                )
            })?;
        action.status = status;
        action.reason = diagnostic.message.clone();
        action.diagnostics.push(diagnostic.clone());
        self.history.push(AgentTaskLoopHistoryEvent {
            event_id: format!("runner-policy-block-{}", self.history.len() + 1),
            event_type: "runner_policy.blocked".to_string(),
            recorded_at: now_timestamp(),
            entity_id: action_entity_id(&action.action),
            payload: json!({
                "action_id": action_id,
                "status": status,
                "diagnostic": diagnostic,
            }),
        });
        self.touch();
        Ok(())
    }

    pub fn resolve_action_runner_policy<F>(
        &self,
        action: &AgentTaskLoopPolicyAction,
        mut runner_availability: F,
    ) -> AgentTaskLoopRunnerPolicyDecision
    where
        F: FnMut(&str) -> AgentTaskLoopRunnerAvailability,
    {
        let policy = runner_policy_for_action(action);
        let fallback = policy.local_fallback.unwrap_or_else(|| {
            if policy.runner.is_some() {
                AgentTaskLoopLocalFallbackPolicy::Denied
            } else {
                AgentTaskLoopLocalFallbackPolicy::Allowed
            }
        });

        let Some(runner) = policy.runner else {
            return match fallback {
                AgentTaskLoopLocalFallbackPolicy::Allowed => AgentTaskLoopRunnerPolicyDecision {
                    target: Some(AgentTaskLoopRunnerExecutionTarget::Local),
                    blocked_status: None,
                    diagnostic: None,
                },
                AgentTaskLoopLocalFallbackPolicy::Denied => blocked_runner_decision(
                    AgentTaskLoopActionStatus::BlockedLocalFallbackDenied,
                    None,
                    "controller action denies local fallback but did not declare a runner",
                    Value::Null,
                ),
            };
        };

        match runner_availability(&runner) {
            AgentTaskLoopRunnerAvailability::Available => AgentTaskLoopRunnerPolicyDecision {
                target: Some(AgentTaskLoopRunnerExecutionTarget::Runner(runner)),
                blocked_status: None,
                diagnostic: None,
            },
            AgentTaskLoopRunnerAvailability::Unavailable { reason } => match fallback {
                AgentTaskLoopLocalFallbackPolicy::Allowed => AgentTaskLoopRunnerPolicyDecision {
                    target: Some(AgentTaskLoopRunnerExecutionTarget::Local),
                    blocked_status: None,
                    diagnostic: Some(AgentTaskLoopActionDiagnostic {
                        code: "runner_unavailable_local_fallback_allowed".to_string(),
                        message: reason,
                        runner: Some(runner),
                        details: Value::Null,
                    }),
                },
                AgentTaskLoopLocalFallbackPolicy::Denied => blocked_runner_decision(
                    AgentTaskLoopActionStatus::BlockedRunnerUnavailable,
                    Some(runner),
                    reason,
                    Value::Null,
                ),
            },
            AgentTaskLoopRunnerAvailability::MaterializationBlocked { reason } => match fallback {
                AgentTaskLoopLocalFallbackPolicy::Allowed => AgentTaskLoopRunnerPolicyDecision {
                    target: Some(AgentTaskLoopRunnerExecutionTarget::Local),
                    blocked_status: None,
                    diagnostic: Some(AgentTaskLoopActionDiagnostic {
                        code: "remote_materialization_blocked_local_fallback_allowed".to_string(),
                        message: reason,
                        runner: Some(runner),
                        details: Value::Null,
                    }),
                },
                AgentTaskLoopLocalFallbackPolicy::Denied => blocked_runner_decision(
                    AgentTaskLoopActionStatus::BlockedRemoteMaterialization,
                    Some(runner),
                    reason,
                    Value::Null,
                ),
            },
        }
    }

    pub fn mark_human_ready(&mut self, entity_id: &str, reason: Option<String>) -> Result<()> {
        let entity = self.entities.get_mut(entity_id).ok_or_else(|| {
            Error::validation_invalid_argument(
                "entity_id",
                format!("loop entity '{entity_id}' does not exist"),
                Some(entity_id.to_string()),
                None,
            )
        })?;
        entity.human_ready = true;
        entity.state = Some("human_ready".to_string());
        self.state = AgentTaskLoopControllerState::HumanReady;
        self.history.push(AgentTaskLoopHistoryEvent {
            event_id: format!("human-ready-{}", self.history.len() + 1),
            event_type: "human_ready".to_string(),
            recorded_at: now_timestamp(),
            entity_id: Some(entity_id.to_string()),
            payload: json!({ "reason": reason }),
        });
        self.touch();
        Ok(())
    }

    // Part of the loop-controller API exercised only by tests; production wiring is pending.
    #[cfg(test)]
    pub(crate) fn route_finding_packet(
        &mut self,
        finding: AgentTaskLoopFindingPacket,
        request_template: Value,
    ) -> AgentTaskLoopPolicyActionRecord {
        let dedupe_key = format!(
            "finding:{}",
            finding
                .reproduction_key
                .as_deref()
                .unwrap_or(&finding.finding_id)
        );
        if self.dedupe_keys.contains_key(&dedupe_key) {
            return self.record_action(
                AgentTaskLoopPolicyAction::RouteFinding {
                    finding,
                    dedupe_key,
                    entity_id: None,
                    request_template,
                },
                "finding packet route already satisfied",
            );
        }

        let entity_id = self.upsert_entity(
            "finding",
            finding
                .reproduction_key
                .as_deref()
                .unwrap_or(&finding.finding_id),
            Vec::new(),
            json!({
                "severity": finding.severity.clone(),
                "owner": finding.owner.clone(),
                "source_transformer": finding.source_transformer.clone(),
            }),
        );
        if let Some(entity) = self.entities.get_mut(&entity_id) {
            entity.artifact_refs.extend(finding.lineage.clone());
            entity
                .provenance
                .extend(finding.lineage.iter().map(|artifact| {
                    AgentTaskLoopProvenanceRef {
                        kind: artifact
                            .kind
                            .clone()
                            .unwrap_or_else(|| "artifact".to_string()),
                        uri: artifact.uri.clone(),
                        caused_by: Some(finding.finding_id.clone()),
                    }
                }));
        }
        self.record_action(
            AgentTaskLoopPolicyAction::RouteFinding {
                finding,
                dedupe_key,
                entity_id: Some(entity_id),
                request_template,
            },
            "finding packet routed to follow-up task",
        )
    }

    // Part of the loop-controller API exercised only by tests; production wiring is pending.
    #[cfg(test)]
    pub(crate) fn record_candidate_patch_validation(
        &mut self,
        candidate: AgentTaskLoopCandidatePatch,
        validation: AgentTaskLoopCandidateValidation,
        limits: AgentTaskLoopCandidateLoopLimits,
    ) -> AgentTaskLoopPolicyActionRecord {
        let entity_id = self.upsert_entity(
            "candidate_patch",
            &candidate.candidate_id,
            candidate.finding_id.clone().into_iter().collect(),
            json!({
                "worktree": candidate.worktree.clone(),
                "attempt": candidate.attempt,
                "finding_id": candidate.finding_id.clone(),
            }),
        );
        if let Some(entity) = self.entities.get_mut(&entity_id) {
            entity.artifact_refs.push(candidate.patch.clone());
            entity.artifact_refs.extend(candidate.lineage.clone());
            entity.artifact_refs.extend(validation.evidence.clone());
        }

        self.record_action(
            AgentTaskLoopPolicyAction::ValidateCandidatePatch {
                candidate,
                validation,
                limits,
            },
            "candidate patch validation recorded",
        )
    }

    pub fn record_pr_ownership_status(
        &mut self,
        request: &AgentTaskPrOwnershipRequest,
        entity_id: Option<String>,
        status: AgentTaskPrOwnershipStatusUpdate,
    ) -> AgentTaskPrOwnershipRecord {
        let state = pr_ownership_state_from_status(&status, request);
        let record = AgentTaskPrOwnershipRecord {
            ownership_id: request.ownership_id.clone(),
            entity_id: entity_id.clone(),
            state,
            base: request.base.clone(),
            head: request.head.clone(),
            pr_number: status.pr_number.or(request.pr_number),
            pr_url: status.pr_url.clone().or_else(|| request.pr_url.clone()),
            head_sha: status.head_sha.clone(),
            ci_state: status.ci_state.clone(),
            ci_summary: status.ci_summary.clone(),
            review_decision: status.review_decision.clone(),
            merge_state: status.merge_state.clone(),
            retry_count: status.retry_count,
            max_retries: request.max_retries,
            merge_required: request.merge_required,
            last_checked_at: Some(now_timestamp()),
            evidence: status.evidence.clone(),
        };

        if let Some(existing) = self
            .pr_ownerships
            .iter_mut()
            .find(|existing| existing.ownership_id == record.ownership_id)
        {
            *existing = record.clone();
        } else {
            self.pr_ownerships.push(record.clone());
        }

        if let Some(entity_id) = &entity_id {
            if let Some(entity) = self.entities.get_mut(entity_id) {
                entity.state = Some(format!("pr_{:?}", state).to_ascii_lowercase());
                entity.metadata = merge_json_object(
                    entity.metadata.clone(),
                    json!({
                        "pr_ownership": {
                            "ownership_id": record.ownership_id,
                            "pr_number": record.pr_number,
                            "pr_url": record.pr_url,
                            "head": record.head,
                            "ci_state": record.ci_state,
                            "merge_state": record.merge_state,
                            "review_decision": record.review_decision,
                            "state": state,
                        }
                    }),
                );
            }
        }

        self.history.push(AgentTaskLoopHistoryEvent {
            event_id: format!("pr-ownership-{}", self.history.len() + 1),
            event_type: "github.pr.ownership_status".to_string(),
            recorded_at: now_timestamp(),
            entity_id,
            payload: json!({ "ownership": record }),
        });
        self.touch();
        record
    }

    pub fn record_terminal_outcome(
        &mut self,
        status: AgentTaskLoopTerminalStatus,
        reason: impl Into<String>,
        action_id: Option<String>,
        entity_id: Option<String>,
        details: Value,
    ) -> AgentTaskLoopTerminalOutcome {
        let outcome = AgentTaskLoopTerminalOutcome {
            outcome_id: format!("terminal-outcome-{}", self.terminal_outcomes.len() + 1),
            status,
            reason: reason.into(),
            action_id,
            entity_id,
            details,
            recorded_at: now_timestamp(),
        };
        self.history.push(AgentTaskLoopHistoryEvent {
            event_id: format!("terminal-outcome-{}", self.history.len() + 1),
            event_type: "controller.terminal_outcome.recorded".to_string(),
            recorded_at: now_timestamp(),
            entity_id: outcome.entity_id.clone(),
            payload: json!({ "outcome": outcome.clone() }),
        });
        self.terminal_outcomes.push(outcome.clone());
        self.touch();
        outcome
    }

    fn apply_action_side_effects(
        &mut self,
        action: &AgentTaskLoopPolicyAction,
        status: AgentTaskLoopActionStatus,
        action_id: &str,
    ) {
        if status == AgentTaskLoopActionStatus::AlreadySatisfied {
            return;
        }
        match action {
            AgentTaskLoopPolicyAction::RouteFinding { entity_id, .. } => {
                if let Some(entity_id) = entity_id {
                    if let Some(entity) = self.entities.get_mut(entity_id) {
                        entity.state = Some("routed".to_string());
                    }
                }
            }
            AgentTaskLoopPolicyAction::ValidateCandidatePatch {
                candidate,
                validation,
                limits,
            } => {
                let entity_id = format!(
                    "candidate_patch:{}",
                    sanitize_loop_id(&candidate.candidate_id)
                );
                if let Some(entity) = self.entities.get_mut(&entity_id) {
                    match validation.status {
                        AgentTaskLoopCandidateValidationStatus::Passed => {
                            entity.state = Some("validated".to_string());
                            entity.human_ready = true;
                            self.state = AgentTaskLoopControllerState::HumanReady;
                        }
                        AgentTaskLoopCandidateValidationStatus::Failed
                            if candidate.attempt >= limits.max_attempts =>
                        {
                            entity.state = Some("retry_limit_reached".to_string());
                            entity.human_ready = true;
                            self.state = AgentTaskLoopControllerState::HumanReady;
                        }
                        AgentTaskLoopCandidateValidationStatus::Failed => {
                            entity.state = Some("needs_retry".to_string());
                        }
                    }
                }
            }
            AgentTaskLoopPolicyAction::OwnPrUntilGreen {
                ownership,
                entity_id,
            } => {
                let key = format!(
                    "{}#{}",
                    ownership.head,
                    ownership.pr_number.unwrap_or_default()
                );
                let pr_entity_id = entity_id.clone().unwrap_or_else(|| {
                    self.upsert_entity(
                        "pull_request",
                        key,
                        Vec::new(),
                        json!({
                            "ownership_id": ownership.ownership_id,
                            "base": ownership.base,
                            "head": ownership.head,
                            "pr_number": ownership.pr_number,
                            "pr_url": ownership.pr_url,
                        }),
                    )
                });
                self.record_pr_ownership_status(
                    ownership,
                    Some(pr_entity_id),
                    AgentTaskPrOwnershipStatusUpdate::tracking(),
                );
            }
            AgentTaskLoopPolicyAction::WaitForEvent(wait) => {
                self.state = AgentTaskLoopControllerState::Waiting;
                if !self
                    .waits
                    .iter()
                    .any(|existing| existing.wait_key == wait.wait_key)
                {
                    self.waits.push(wait.clone());
                }
            }
            AgentTaskLoopPolicyAction::SpawnController {
                dedupe_key,
                loop_id,
                entity_id,
                request,
                ..
            }
            | AgentTaskLoopPolicyAction::SpawnSubloop {
                dedupe_key,
                loop_id,
                entity_id,
                request,
                ..
            } => {
                self.record_subcontroller_ref(
                    loop_id,
                    dedupe_key,
                    entity_id.clone(),
                    Some(action_id.to_string()),
                    None,
                    Vec::new(),
                    request.clone(),
                );
            }
            AgentTaskLoopPolicyAction::WaitForController {
                loop_id,
                entity_id,
                wait_key,
                terminal_states,
            } => {
                self.state = AgentTaskLoopControllerState::Waiting;
                let wait_key = wait_key
                    .clone()
                    .unwrap_or_else(|| controller_wait_key(loop_id));
                let terminal_states = controller_terminal_states(terminal_states);
                self.record_subcontroller_ref(
                    loop_id,
                    &format!("controller:{loop_id}"),
                    entity_id.clone(),
                    None,
                    Some(wait_key.clone()),
                    terminal_states.clone(),
                    Value::Null,
                );
                if !self
                    .waits
                    .iter()
                    .any(|existing| existing.wait_key == wait_key)
                {
                    self.waits.push(AgentTaskLoopWait {
                        wait_key,
                        event_type: "controller.terminal".to_string(),
                        entity_id: entity_id.clone(),
                        external_ref: Some(loop_id.clone()),
                        timeout_at: None,
                        escalation_policy: None,
                        status: AgentTaskLoopWaitStatus::Open,
                        satisfied_by_event_id: None,
                    });
                }
            }
            AgentTaskLoopPolicyAction::MarkHumanReady { entity_id, reason } => {
                let _ = self.mark_human_ready(entity_id, reason.clone());
            }
            _ => {}
        }
    }

    fn open_wait_count(&self) -> usize {
        self.waits
            .iter()
            .filter(|wait| wait.status == AgentTaskLoopWaitStatus::Open)
            .count()
    }

    fn touch(&mut self) {
        self.updated_at = now_timestamp();
    }

    fn record_subcontroller_ref(
        &mut self,
        loop_id: &str,
        dedupe_key: &str,
        entity_id: Option<String>,
        parent_action_id: Option<String>,
        wait_key: Option<String>,
        terminal_states: Vec<AgentTaskLoopControllerState>,
        request: Value,
    ) {
        if let Some(existing) = self
            .subcontrollers
            .iter_mut()
            .find(|existing| existing.dedupe_key == dedupe_key || existing.loop_id == loop_id)
        {
            existing.entity_id = existing.entity_id.clone().or(entity_id);
            existing.parent_action_id = existing.parent_action_id.clone().or(parent_action_id);
            existing.wait_key = existing.wait_key.clone().or(wait_key);
            if existing.terminal_states.is_empty() {
                existing.terminal_states = terminal_states;
            }
            if existing.request.is_null() {
                existing.request = request;
            }
            existing.updated_at = now_timestamp();
            return;
        }

        let now = now_timestamp();
        self.subcontrollers.push(AgentTaskLoopSubcontrollerRef {
            loop_id: sanitize_loop_id(loop_id),
            dedupe_key: dedupe_key.to_string(),
            entity_id,
            parent_loop_id: Some(self.loop_id.clone()),
            parent_action_id,
            wait_key,
            terminal_states,
            state: None,
            created_at: now.clone(),
            updated_at: now,
            request,
        });
    }
}

pub fn controller_status_report(loop_id: &str) -> Result<AgentTaskLoopControllerStatusReport> {
    let controller = controller_status(loop_id)?;
    let diagnostics = controller_status_diagnostics(&controller)?;
    Ok(AgentTaskLoopControllerStatusReport {
        schema: AGENT_TASK_LOOP_CONTROLLER_STATUS_SCHEMA.to_string(),
        controller,
        diagnostics,
    })
}

pub fn controller_status_diagnostics(
    record: &AgentTaskLoopControllerRecord,
) -> Result<AgentTaskLoopControllerDiagnostics> {
    controller_status_diagnostics_with(record, Utc::now(), |run_id| {
        agent_task_lifecycle::run_record_exists(run_id)
    })
}

fn controller_status_diagnostics_with<F>(
    record: &AgentTaskLoopControllerRecord,
    now: DateTime<Utc>,
    mut run_exists: F,
) -> Result<AgentTaskLoopControllerDiagnostics>
where
    F: FnMut(&str) -> Result<bool>,
{
    let mut pending_actions = Vec::new();
    let mut stale_pending_action_count = 0;
    let mut orphaned_pending_action_count = 0;

    for action in record
        .next_actions
        .iter()
        .filter(|action| action.status == AgentTaskLoopActionStatus::Pending)
    {
        let age_seconds = parse_timestamp(&action.created_at).map(|created_at| {
            now.signed_duration_since(created_at.with_timezone(&Utc))
                .num_seconds()
                .max(0)
        });
        let stale = age_seconds.is_some_and(|age| age >= STALE_PENDING_ACTION_SECONDS);
        let runner_id = action_runner_id(action, record);
        let referenced_run_id = action_referenced_run_id(action, record);
        let missing_referenced_run = if let Some(run_id) = referenced_run_id.as_deref() {
            !run_exists(run_id)?
        } else {
            false
        };
        let orphaned = missing_referenced_run;
        let mut problems = Vec::new();
        if stale {
            problems.push("pending action is older than stale threshold".to_string());
        }
        if missing_referenced_run {
            problems.push("referenced run record is missing".to_string());
        }
        let recovery_commands = if stale || orphaned {
            recovery_commands_for(record, runner_id.as_deref())
        } else {
            Vec::new()
        };

        if stale {
            stale_pending_action_count += 1;
        }
        if orphaned {
            orphaned_pending_action_count += 1;
        }
        pending_actions.push(AgentTaskLoopPendingActionDiagnostic {
            action_id: action.action_id.clone(),
            action: action_name(&action.action).to_string(),
            dedupe_key: action.dedupe_key.clone(),
            runner_id,
            referenced_run_id,
            created_at: action.created_at.clone(),
            age_seconds,
            stale,
            orphaned,
            problems,
            recovery_commands,
        });
    }

    Ok(AgentTaskLoopControllerDiagnostics {
        schema: "homeboy/agent-task-loop-controller-diagnostics/v1".to_string(),
        stale_pending_threshold_seconds: STALE_PENDING_ACTION_SECONDS,
        summary: AgentTaskLoopControllerDiagnosticSummary {
            pending_action_count: pending_actions.len(),
            stale_pending_action_count,
            orphaned_pending_action_count,
        },
        pending_actions,
    })
}

pub fn create_controller(
    loop_id: &str,
    phase: &str,
    config_version: &str,
) -> Result<AgentTaskLoopControllerRecord> {
    let record = AgentTaskLoopControllerRecord::new(loop_id, phase, config_version);
    write_controller(&record)?;
    Ok(record)
}

pub fn load_controller(loop_id: &str) -> Result<AgentTaskLoopControllerRecord> {
    read_json(&controller_path(&sanitize_loop_id(loop_id))?)
}

pub fn controller_status(loop_id: &str) -> Result<AgentTaskLoopControllerRecord> {
    let mut record = load_controller(loop_id)?;
    let refreshed_child_runs = refresh_stale_running_child_actions(&mut record)?;
    let refreshed_subcontrollers = refresh_subcontroller_statuses(&mut record)?;
    if refreshed_child_runs || refreshed_subcontrollers {
        write_controller(&record)?;
    }
    Ok(record)
}

pub fn list_controllers() -> Result<Vec<AgentTaskLoopControllerRecord>> {
    let root = controllers_root()?;
    let entries = match fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(Error::internal_io(
                error.to_string(),
                Some(root.display().to_string()),
            ));
        }
    };
    let mut records = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| {
            Error::internal_io(error.to_string(), Some(root.display().to_string()))
        })?;
        let path = entry.path().join("controller.json");
        if path.exists() {
            records.push(read_json(&path)?);
        }
    }
    records.sort_by(|left: &AgentTaskLoopControllerRecord, right| left.loop_id.cmp(&right.loop_id));
    Ok(records)
}

pub fn write_controller(record: &AgentTaskLoopControllerRecord) -> Result<()> {
    write_json(&controller_path(&record.loop_id)?, record)
}

pub fn apply_external_event(
    loop_id: &str,
    event: AgentTaskLoopExternalEvent,
) -> Result<AgentTaskLoopControllerRecord> {
    let mut record = load_controller(loop_id)?;
    record.apply_event(event);
    write_controller(&record)?;
    Ok(record)
}

fn read_json<T: serde::de::DeserializeOwned>(path: &PathBuf) -> Result<T> {
    let raw = fs::read_to_string(path)
        .map_err(|error| Error::internal_io(error.to_string(), Some(path.display().to_string())))?;
    serde_json::from_str(&raw)
        .map_err(|error| Error::internal_json(error.to_string(), Some(path.display().to_string())))
}

fn write_json<T: Serialize>(path: &PathBuf, value: &T) -> Result<()> {
    let parent = path.parent().ok_or_else(|| {
        Error::internal_unexpected(format!("path has no parent: {}", path.display()))
    })?;
    fs::create_dir_all(parent).map_err(|error| {
        Error::internal_io(error.to_string(), Some(parent.display().to_string()))
    })?;
    let json = serde_json::to_string_pretty(value).map_err(|error| {
        Error::internal_json(error.to_string(), Some(path.display().to_string()))
    })?;
    fs::write(path, format!("{json}\n"))
        .map_err(|error| Error::internal_io(error.to_string(), Some(path.display().to_string())))
}

fn controller_path(loop_id: &str) -> Result<PathBuf> {
    Ok(controllers_root()?
        .join(sanitize_loop_id(loop_id))
        .join("controller.json"))
}

fn controllers_root() -> Result<PathBuf> {
    Ok(paths::homeboy_data()?.join("agent-task-loops"))
}

fn action_dedupe_key(action: &AgentTaskLoopPolicyAction) -> Option<String> {
    match action {
        AgentTaskLoopPolicyAction::SpawnTask { dedupe_key, .. }
        | AgentTaskLoopPolicyAction::FanOut { dedupe_key, .. }
        | AgentTaskLoopPolicyAction::SpawnController { dedupe_key, .. }
        | AgentTaskLoopPolicyAction::SpawnSubloop { dedupe_key, .. }
        | AgentTaskLoopPolicyAction::RouteFinding { dedupe_key, .. } => Some(dedupe_key.clone()),
        AgentTaskLoopPolicyAction::ValidateCandidatePatch {
            candidate,
            validation,
            ..
        } => Some(format!(
            "candidate-validation:{}:{}",
            candidate.candidate_id, validation.validation_id
        )),
        AgentTaskLoopPolicyAction::WaitForEvent(wait) => Some(format!("wait:{}", wait.wait_key)),
        AgentTaskLoopPolicyAction::WaitForController {
            loop_id, wait_key, ..
        } => Some(format!(
            "wait:{}",
            wait_key
                .clone()
                .unwrap_or_else(|| controller_wait_key(loop_id))
        )),
        AgentTaskLoopPolicyAction::RunGates {
            bundle_id,
            entity_id,
        } => entity_id
            .as_ref()
            .map(|entity_id| format!("gate:{bundle_id}:{entity_id}")),
        AgentTaskLoopPolicyAction::OwnPrUntilGreen { ownership, .. } => {
            Some(format!("pr-ownership:{}", ownership.ownership_id))
        }
        AgentTaskLoopPolicyAction::RequestChanges {
            target_run_id,
            feedback_id,
        } => Some(format!(
            "feedback:{}:{}",
            target_run_id,
            feedback_id.as_deref().unwrap_or("latest")
        )),
        _ => None,
    }
}

fn jsonpath_match_is_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(value) => value.as_i64() != Some(0) || value.as_u64().is_some_and(|n| n > 0),
        Value::String(value) => !value.is_empty(),
        Value::Array(value) => !value.is_empty(),
        Value::Object(value) => !value.is_empty(),
    }
}

pub(crate) fn action_entity_id(action: &AgentTaskLoopPolicyAction) -> Option<String> {
    match action {
        AgentTaskLoopPolicyAction::SpawnTask { entity_id, .. }
        | AgentTaskLoopPolicyAction::SpawnController { entity_id, .. }
        | AgentTaskLoopPolicyAction::SpawnSubloop { entity_id, .. }
        | AgentTaskLoopPolicyAction::WaitForController { entity_id, .. }
        | AgentTaskLoopPolicyAction::RouteFinding { entity_id, .. }
        | AgentTaskLoopPolicyAction::RunGates { entity_id, .. }
        | AgentTaskLoopPolicyAction::OwnPrUntilGreen { entity_id, .. } => entity_id.clone(),
        AgentTaskLoopPolicyAction::MarkHumanReady { entity_id, .. } => Some(entity_id.clone()),
        _ => None,
    }
}

fn action_name(action: &AgentTaskLoopPolicyAction) -> &'static str {
    match action {
        AgentTaskLoopPolicyAction::SpawnTask { .. } => "spawn_task",
        AgentTaskLoopPolicyAction::FanOut { .. } => "fan_out",
        AgentTaskLoopPolicyAction::SpawnController { .. } => "spawn_controller",
        AgentTaskLoopPolicyAction::SpawnSubloop { .. } => "spawn_subloop",
        AgentTaskLoopPolicyAction::RouteFinding { .. } => "route_finding",
        AgentTaskLoopPolicyAction::ValidateCandidatePatch { .. } => "validate_candidate_patch",
        AgentTaskLoopPolicyAction::Join { .. } => "join",
        AgentTaskLoopPolicyAction::Retry { .. } => "retry",
        AgentTaskLoopPolicyAction::RequestChanges { .. } => "request_changes",
        AgentTaskLoopPolicyAction::RunGates { .. } => "run_gates",
        AgentTaskLoopPolicyAction::OwnPrUntilGreen { .. } => "own_pr_until_green",
        AgentTaskLoopPolicyAction::WaitForEvent(_) => "wait_for_event",
        AgentTaskLoopPolicyAction::WaitForController { .. } => "wait_for_controller",
        AgentTaskLoopPolicyAction::MarkHumanReady { .. } => "mark_human_ready",
        AgentTaskLoopPolicyAction::Complete { .. } => "complete",
        AgentTaskLoopPolicyAction::Abandon { .. } => "abandon",
        AgentTaskLoopPolicyAction::Escalate { .. } => "escalate",
    }
}

fn action_runner_id(
    action: &AgentTaskLoopPolicyActionRecord,
    record: &AgentTaskLoopControllerRecord,
) -> Option<String> {
    action_value(action)
        .as_ref()
        .and_then(|value| first_string_at_keys(value, &["runner_id", "runner", "lab_runner_id"]))
        .or_else(|| {
            first_string_at_keys(
                &record.metadata,
                &["runner_id", "runner", "lab_runner_id", "configured_runner"],
            )
        })
}

fn action_referenced_run_id(
    action: &AgentTaskLoopPolicyActionRecord,
    record: &AgentTaskLoopControllerRecord,
) -> Option<String> {
    match &action.action {
        AgentTaskLoopPolicyAction::Retry { target_run_id }
        | AgentTaskLoopPolicyAction::RequestChanges { target_run_id, .. } => {
            Some(target_run_id.clone())
        }
        _ => action_value(action)
            .as_ref()
            .and_then(|value| {
                first_string_at_keys(
                    value,
                    &[
                        "referenced_run_id",
                        "target_run_id",
                        "remote_run_id",
                        "run_id",
                    ],
                )
            })
            .or_else(|| referenced_run_id_from_dedupe(action, record)),
    }
}

fn referenced_run_id_from_dedupe(
    action: &AgentTaskLoopPolicyActionRecord,
    record: &AgentTaskLoopControllerRecord,
) -> Option<String> {
    let dedupe_key = action.dedupe_key.as_ref()?;
    record
        .dedupe_keys
        .get(dedupe_key)
        .and_then(|dedupe| dedupe.run_id.clone())
}

fn action_value(action: &AgentTaskLoopPolicyActionRecord) -> Option<Value> {
    serde_json::to_value(&action.action).ok()
}

fn first_string_at_keys(value: &Value, keys: &[&str]) -> Option<String> {
    match value {
        Value::Object(map) => {
            for key in keys {
                if let Some(value) = map.get(*key).and_then(Value::as_str) {
                    if !value.trim().is_empty() {
                        return Some(value.to_string());
                    }
                }
            }
            map.values()
                .find_map(|value| first_string_at_keys(value, keys))
        }
        Value::Array(values) => values
            .iter()
            .find_map(|value| first_string_at_keys(value, keys)),
        _ => None,
    }
}

fn parse_timestamp(value: &str) -> Option<DateTime<chrono::FixedOffset>> {
    DateTime::parse_from_rfc3339(value).ok()
}

fn recovery_commands_for(
    record: &AgentTaskLoopControllerRecord,
    runner_id: Option<&str>,
) -> Vec<String> {
    let loop_id = shell_arg(&record.loop_id);
    let mut commands = Vec::new();
    if let Some(runner_id) = runner_id {
        commands.push(format!(
            "homeboy agent-task controller run {loop_id} --runner {}",
            shell_arg(runner_id)
        ));
        commands.push(format!(
            "homeboy agent-task controller resume {loop_id} --runner {}",
            shell_arg(runner_id)
        ));
    } else {
        commands.push(format!("homeboy agent-task controller run {loop_id}"));
        commands.push(format!("homeboy agent-task controller resume {loop_id}"));
    }
    commands
}

fn shell_arg(value: &str) -> String {
    if value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | '/' | ':'))
    {
        value.to_string()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

fn refresh_subcontroller_statuses(record: &mut AgentTaskLoopControllerRecord) -> Result<bool> {
    let mut changed = false;
    let mut satisfied_waits = Vec::new();
    for subcontroller in &mut record.subcontrollers {
        let Ok(child) = load_controller(&subcontroller.loop_id) else {
            continue;
        };
        if subcontroller.state != Some(child.state) {
            subcontroller.state = Some(child.state);
            subcontroller.updated_at = now_timestamp();
            changed = true;
        }
        let terminal_states = controller_terminal_states(&subcontroller.terminal_states);
        if terminal_states.contains(&child.state) {
            if let Some(wait_key) = &subcontroller.wait_key {
                satisfied_waits.push((wait_key.clone(), child.loop_id.clone(), child.state));
            }
        }
    }

    for (wait_key, child_loop_id, child_state) in satisfied_waits {
        if let Some(wait) = record
            .waits
            .iter_mut()
            .find(|wait| wait.wait_key == wait_key && wait.status == AgentTaskLoopWaitStatus::Open)
        {
            wait.status = AgentTaskLoopWaitStatus::Satisfied;
            wait.satisfied_by_event_id = Some(format!(
                "controller-terminal:{child_loop_id}:{child_state:?}"
            ));
            changed = true;
        }
    }

    if record.open_wait_count() == 0 && record.state == AgentTaskLoopControllerState::Waiting {
        record.state = AgentTaskLoopControllerState::Running;
        changed = true;
    }

    if changed {
        record.touch();
    }
    Ok(changed)
}

fn refresh_stale_running_child_actions(record: &mut AgentTaskLoopControllerRecord) -> Result<bool> {
    let mut changed = false;
    let mut history_events = Vec::new();

    for index in 0..record.next_actions.len() {
        let action = &record.next_actions[index];
        if action.status != AgentTaskLoopActionStatus::Running
            || !matches!(action.action, AgentTaskLoopPolicyAction::SpawnTask { .. })
        {
            continue;
        }

        let Some(run_id) = action_referenced_run_id(action, record) else {
            continue;
        };
        let run = agent_task_lifecycle::status(&run_id)?;
        if run.state != AgentTaskRunState::Running
            || run.metadata.get("stale_running").and_then(Value::as_bool) != Some(true)
        {
            continue;
        }

        let reason = run
            .metadata
            .get("stale_running_reason")
            .and_then(Value::as_str)
            .unwrap_or("stale_running");
        let action = &mut record.next_actions[index];
        action.status = AgentTaskLoopActionStatus::Pending;
        action.reason = format!(
            "child agent-task run '{run_id}' is stale ({reason}); action reset for recovery"
        );
        action.diagnostics.push(AgentTaskLoopActionDiagnostic {
            code: "stale_child_run_recovery".to_string(),
            message: action.reason.clone(),
            runner: None,
            details: json!({
                "run_id": run_id,
                "stale_running_reason": reason,
            }),
        });
        history_events.push((
            action.action_id.clone(),
            action.dedupe_key.clone(),
            run_id,
            reason.to_string(),
        ));
        changed = true;
    }

    for (action_id, dedupe_key, run_id, reason) in history_events {
        record.history.push(AgentTaskLoopHistoryEvent {
            event_id: format!("stale-child-recovery-{}", record.history.len() + 1),
            event_type: "controller.action.stale_child_recovery".to_string(),
            recorded_at: now_timestamp(),
            entity_id: None,
            payload: json!({
                "action_id": action_id,
                "dedupe_key": dedupe_key,
                "run_id": run_id,
                "stale_running_reason": reason,
            }),
        });
    }

    if changed {
        record.touch();
    }
    Ok(changed)
}

fn controller_wait_key(loop_id: &str) -> String {
    format!("controller:{}:terminal", sanitize_loop_id(loop_id))
}

fn controller_terminal_states(
    states: &[AgentTaskLoopControllerState],
) -> Vec<AgentTaskLoopControllerState> {
    if states.is_empty() {
        vec![
            AgentTaskLoopControllerState::Completed,
            AgentTaskLoopControllerState::Failed,
            AgentTaskLoopControllerState::HumanReady,
            AgentTaskLoopControllerState::Abandoned,
            AgentTaskLoopControllerState::Escalated,
        ]
    } else {
        states.to_vec()
    }
}

fn default_controller_phase() -> String {
    "init".to_string()
}

fn default_config_version() -> String {
    "v1".to_string()
}

fn entity_dedupe_key(entity_type: &str, key: &str) -> String {
    format!("entity:{entity_type}:{key}")
}

fn sanitize_loop_id(raw: &str) -> String {
    paths::sanitize_path_segment(raw)
}

fn controller_schema() -> String {
    AGENT_TASK_LOOP_CONTROLLER_SCHEMA.to_string()
}

fn open_wait_status() -> AgentTaskLoopWaitStatus {
    AgentTaskLoopWaitStatus::Open
}

fn default_candidate_max_attempts() -> u32 {
    3
}

fn default_pr_ownership_max_retries() -> u32 {
    3
}

fn pr_ownership_state_from_status(
    status: &AgentTaskPrOwnershipStatusUpdate,
    request: &AgentTaskPrOwnershipRequest,
) -> AgentTaskPrOwnershipState {
    if status.missing_pr {
        return AgentTaskPrOwnershipState::MissingPr;
    }
    if status
        .merge_state
        .as_deref()
        .is_some_and(|state| state.eq_ignore_ascii_case("MERGED"))
    {
        return AgentTaskPrOwnershipState::Merged;
    }
    if status
        .ci_state
        .as_deref()
        .is_some_and(|state| state == "terminal_failed" || state == "stale")
    {
        return if status.retry_count >= request.max_retries {
            AgentTaskPrOwnershipState::RetryLimitReached
        } else {
            AgentTaskPrOwnershipState::ChangesRequested
        };
    }
    if status
        .review_decision
        .as_deref()
        .is_some_and(|decision| decision == "CHANGES_REQUESTED")
    {
        return if status.retry_count >= request.max_retries {
            AgentTaskPrOwnershipState::RetryLimitReached
        } else {
            AgentTaskPrOwnershipState::ChangesRequested
        };
    }
    if status
        .ci_state
        .as_deref()
        .is_some_and(|state| state == "pending" || state == "no_checks" || state == "tracking")
    {
        return AgentTaskPrOwnershipState::WaitingForChecks;
    }
    if status
        .ci_state
        .as_deref()
        .is_some_and(|state| state == "terminal_green")
    {
        return if request.merge_required {
            AgentTaskPrOwnershipState::WaitingForMerge
        } else {
            AgentTaskPrOwnershipState::GreenReady
        };
    }
    AgentTaskPrOwnershipState::Tracking
}

fn merge_json_object(left: Value, right: Value) -> Value {
    let mut merged = left.as_object().cloned().unwrap_or_default();
    if let Some(right) = right.as_object() {
        for (key, value) in right {
            merged.insert(key.clone(), value.clone());
        }
    }
    Value::Object(merged)
}

fn now_timestamp() -> String {
    Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::with_isolated_home;

    #[test]
    fn controller_persists_and_resumes_loop_state() {
        with_isolated_home(|_| {
            let mut record = create_controller("loop/demo", "generate", "v1").expect("created");
            let entity_id = record.upsert_entity("idea", "site-1", Vec::new(), Value::Null);
            write_controller(&record).expect("written");

            let loaded = load_controller("loop/demo").expect("loaded");

            assert_eq!(loaded.loop_id, "loop_demo");
            assert_eq!(loaded.phase, "generate");
            assert!(loaded.entities.contains_key(&entity_id));
        });
    }

    #[test]
    fn dedupe_keys_prevent_duplicate_spawn_actions() {
        let mut record = AgentTaskLoopControllerRecord::new("loop", "repair", "v1");
        let first = record.record_action(
            AgentTaskLoopPolicyAction::SpawnTask {
                dedupe_key: "finding:abc:repair".to_string(),
                entity_id: Some("finding:abc".to_string()),
                request: json!({ "task": "repair" }),
            },
            "finding emitted",
        );
        let second = record.record_action(
            AgentTaskLoopPolicyAction::SpawnTask {
                dedupe_key: "finding:abc:repair".to_string(),
                entity_id: Some("finding:abc".to_string()),
                request: json!({ "task": "repair" }),
            },
            "resume replay",
        );

        assert_eq!(first.status, AgentTaskLoopActionStatus::Pending);
        assert_eq!(second.status, AgentTaskLoopActionStatus::AlreadySatisfied);
        assert_eq!(record.dedupe_keys.len(), 1);
    }

    #[test]
    fn status_diagnostics_flag_old_pending_actions() {
        let mut record = AgentTaskLoopControllerRecord::new("loop", "repair", "v1");
        record.metadata = json!({ "runner_id": "lab-runner" });
        record.record_action(
            AgentTaskLoopPolicyAction::SpawnTask {
                dedupe_key: "finding:abc:repair".to_string(),
                entity_id: Some("finding:abc".to_string()),
                request: json!({ "task": "repair" }),
            },
            "finding emitted",
        );
        record.next_actions[0].created_at = "2026-06-09T00:00:00Z".to_string();

        let diagnostics = controller_status_diagnostics_with(
            &record,
            DateTime::parse_from_rfc3339("2026-06-11T00:00:01Z")
                .expect("now")
                .with_timezone(&Utc),
            |_| Ok(true),
        )
        .expect("diagnostics");

        assert_eq!(diagnostics.summary.pending_action_count, 1);
        assert_eq!(diagnostics.summary.stale_pending_action_count, 1);
        assert_eq!(diagnostics.summary.orphaned_pending_action_count, 0);
        let action = &diagnostics.pending_actions[0];
        assert_eq!(action.action_id, "action-1");
        assert_eq!(action.dedupe_key.as_deref(), Some("finding:abc:repair"));
        assert_eq!(action.runner_id.as_deref(), Some("lab-runner"));
        assert_eq!(action.age_seconds, Some(172801));
        assert!(action.stale);
        assert!(!action.orphaned);
        assert!(action
            .problems
            .contains(&"pending action is older than stale threshold".to_string()));
        assert!(action.recovery_commands.iter().any(|command| command
            .contains("homeboy agent-task controller run loop --runner lab-runner")));
    }

    #[test]
    fn status_diagnostics_flag_missing_referenced_run_records() {
        let mut record = AgentTaskLoopControllerRecord::new("loop", "repair", "v1");
        record.record_action(
            AgentTaskLoopPolicyAction::SpawnTask {
                dedupe_key: "finding:abc:repair".to_string(),
                entity_id: Some("finding:abc".to_string()),
                request: json!({
                    "task": "repair",
                    "lab": { "runner_id": "configured-lab" },
                    "metadata": { "remote_run_id": "missing-run-123" }
                }),
            },
            "finding emitted",
        );
        record.next_actions[0].created_at = "2026-06-11T00:00:00Z".to_string();

        let diagnostics = controller_status_diagnostics_with(
            &record,
            DateTime::parse_from_rfc3339("2026-06-11T00:05:00Z")
                .expect("now")
                .with_timezone(&Utc),
            |run_id| Ok(run_id != "missing-run-123"),
        )
        .expect("diagnostics");

        assert_eq!(diagnostics.summary.pending_action_count, 1);
        assert_eq!(diagnostics.summary.stale_pending_action_count, 0);
        assert_eq!(diagnostics.summary.orphaned_pending_action_count, 1);
        let action = &diagnostics.pending_actions[0];
        assert_eq!(action.runner_id.as_deref(), Some("configured-lab"));
        assert_eq!(action.referenced_run_id.as_deref(), Some("missing-run-123"));
        assert_eq!(action.age_seconds, Some(300));
        assert!(!action.stale);
        assert!(action.orphaned);
        assert!(action
            .problems
            .contains(&"referenced run record is missing".to_string()));
    }

    #[test]
    fn subcontroller_spawn_records_parent_visible_child_once() {
        let mut record = AgentTaskLoopControllerRecord::new("parent", "plan", "v1");
        let first = record.record_action(
            AgentTaskLoopPolicyAction::SpawnController {
                dedupe_key: "controller:child:plan".to_string(),
                loop_id: "child/controller".to_string(),
                entity_id: Some("goal:4216".to_string()),
                phase: "implement".to_string(),
                config_version: "nested-v1".to_string(),
                request: json!({ "issue": 4216 }),
            },
            "spawn child controller",
        );
        let second = record.record_action(
            AgentTaskLoopPolicyAction::SpawnSubloop {
                dedupe_key: "controller:child:plan".to_string(),
                loop_id: "child/controller".to_string(),
                entity_id: Some("goal:4216".to_string()),
                phase: "implement".to_string(),
                config_version: "nested-v1".to_string(),
                request: json!({ "issue": 4216 }),
            },
            "replayed child controller spawn",
        );

        assert_eq!(first.status, AgentTaskLoopActionStatus::Pending);
        assert_eq!(second.status, AgentTaskLoopActionStatus::AlreadySatisfied);
        assert_eq!(record.subcontrollers.len(), 1);
        let child = &record.subcontrollers[0];
        assert_eq!(child.loop_id, "child_controller");
        assert_eq!(child.parent_loop_id.as_deref(), Some("parent"));
        assert_eq!(child.parent_action_id.as_deref(), Some("action-1"));
        assert_eq!(child.entity_id.as_deref(), Some("goal:4216"));
    }

    #[test]
    fn controller_status_satisfies_wait_when_child_reaches_terminal_state() {
        with_isolated_home(|_| {
            let mut parent = create_controller("parent-loop", "delegate", "v1").expect("parent");
            parent.record_action(
                AgentTaskLoopPolicyAction::SpawnController {
                    dedupe_key: "controller:child-loop".to_string(),
                    loop_id: "child-loop".to_string(),
                    entity_id: Some("goal:4216".to_string()),
                    phase: "implement".to_string(),
                    config_version: "v1".to_string(),
                    request: Value::Null,
                },
                "spawn child",
            );
            parent.record_action(
                AgentTaskLoopPolicyAction::WaitForController {
                    loop_id: "child-loop".to_string(),
                    entity_id: Some("goal:4216".to_string()),
                    wait_key: None,
                    terminal_states: Vec::new(),
                },
                "wait for child terminal state",
            );
            write_controller(&parent).expect("parent written");

            let mut child = create_controller("child-loop", "implement", "v1").expect("child");
            child.state = AgentTaskLoopControllerState::Completed;
            write_controller(&child).expect("child written");

            let refreshed = controller_status("parent-loop").expect("refreshed");

            assert_eq!(refreshed.state, AgentTaskLoopControllerState::Running);
            assert_eq!(
                refreshed.subcontrollers[0].state,
                Some(AgentTaskLoopControllerState::Completed)
            );
            assert_eq!(
                refreshed.waits[0].status,
                AgentTaskLoopWaitStatus::Satisfied
            );
            assert_eq!(
                refreshed.waits[0].satisfied_by_event_id.as_deref(),
                Some("controller-terminal:child-loop:Completed")
            );
        });
    }

    #[test]
    fn external_events_satisfy_matching_waits_and_resume_controller() {
        let mut record = AgentTaskLoopControllerRecord::new("loop", "review", "v1");
        record.record_action(
            AgentTaskLoopPolicyAction::WaitForEvent(AgentTaskLoopWait {
                wait_key: "pr-12-merged".to_string(),
                event_type: "github.pr.merged".to_string(),
                entity_id: Some("pr:12".to_string()),
                external_ref: Some("Extra-Chill/homeboy#12".to_string()),
                timeout_at: None,
                escalation_policy: Some("escalate".to_string()),
                status: AgentTaskLoopWaitStatus::Open,
                satisfied_by_event_id: None,
            }),
            "wait for human merge",
        );

        record.apply_event(AgentTaskLoopExternalEvent {
            event_id: "event-1".to_string(),
            event_type: "github.pr.merged".to_string(),
            event_key: Some("Extra-Chill/homeboy#12".to_string()),
            entity_id: Some("pr:12".to_string()),
            payload: Value::Null,
        });

        assert_eq!(record.state, AgentTaskLoopControllerState::Running);
        assert_eq!(record.waits[0].status, AgentTaskLoopWaitStatus::Satisfied);
        assert_eq!(
            record.waits[0].satisfied_by_event_id.as_deref(),
            Some("event-1")
        );
    }

    #[test]
    fn review_feedback_routes_to_originating_attempt() {
        let mut record = AgentTaskLoopControllerRecord::new("loop", "review", "v1");
        let action = record.record_action(
            AgentTaskLoopPolicyAction::RequestChanges {
                target_run_id: "run-repair-1".to_string(),
                feedback_id: Some("review-1".to_string()),
            },
            "review requested changes",
        );

        assert_eq!(
            action.dedupe_key.as_deref(),
            Some("feedback:run-repair-1:review-1")
        );
        assert_eq!(action.status, AgentTaskLoopActionStatus::Pending);
    }

    #[test]
    fn own_pr_until_green_persists_generic_pr_lifecycle_state() {
        let mut record = AgentTaskLoopControllerRecord::new("loop", "review", "v1");
        let request = AgentTaskPrOwnershipRequest {
            ownership_id: "run-123".to_string(),
            component_id: None,
            path: None,
            base: "main".to_string(),
            head: "fix/pr-owner".to_string(),
            pr_number: Some(42),
            pr_url: Some("https://github.com/Extra-Chill/homeboy/pull/42".to_string()),
            max_retries: 2,
            merge_required: true,
        };
        let action = record.record_action(
            AgentTaskLoopPolicyAction::OwnPrUntilGreen {
                ownership: request.clone(),
                entity_id: None,
            },
            "own PR after finalization",
        );

        assert_eq!(action.status, AgentTaskLoopActionStatus::Pending);
        assert_eq!(record.pr_ownerships.len(), 1);
        assert_eq!(
            record.pr_ownerships[0].state,
            AgentTaskPrOwnershipState::WaitingForChecks
        );
        assert_eq!(record.pr_ownerships[0].head, "fix/pr-owner");
        assert_eq!(record.pr_ownerships[0].pr_number, Some(42));
        assert!(record.entities.contains_key("pull_request:fix_pr-owner_42"));

        let serialized = serde_json::to_value(&action.action).expect("serialized action");
        assert_eq!(serialized["action"], "own_pr_until_green");
        assert_eq!(serialized["ownership"]["ownership_id"], "run-123");
    }

    #[test]
    fn deterministic_policy_transition_can_start_pr_ownership_once() {
        let mut record = AgentTaskLoopControllerRecord::new("loop", "finalize", "v1");
        let ownership = AgentTaskPrOwnershipRequest {
            ownership_id: "branch:fix-pr-owner".to_string(),
            component_id: Some("homeboy".to_string()),
            path: None,
            base: "main".to_string(),
            head: "fix/pr-owner".to_string(),
            pr_number: Some(42),
            pr_url: None,
            max_retries: 3,
            merge_required: false,
        };
        let policy = AgentTaskLoopPolicy {
            policy_id: "own-pr-after-finalization".to_string(),
            transitions: vec![AgentTaskLoopTransition {
                transition_id: "finalized-branch".to_string(),
                from_phase: Some("finalize".to_string()),
                on_event_type: Some("agent_task.finalized".to_string()),
                when_json_path: Some("$.event.payload.branch".to_string()),
                actions: vec![AgentTaskLoopPolicyAction::OwnPrUntilGreen {
                    ownership: ownership.clone(),
                    entity_id: Some("pr:42".to_string()),
                }],
            }],
        };
        let event = AgentTaskLoopExternalEvent {
            event_id: "event-1".to_string(),
            event_type: "agent_task.finalized".to_string(),
            event_key: None,
            entity_id: None,
            payload: json!({ "branch": "fix/pr-owner" }),
        };

        let first = record.evaluate_policy(&policy, Some(&event));
        let second = record.evaluate_policy(&policy, Some(&event));

        assert_eq!(first.len(), 1);
        assert_eq!(first[0].status, AgentTaskLoopActionStatus::Pending);
        assert_eq!(
            first[0].dedupe_key.as_deref(),
            Some("pr-ownership:branch:fix-pr-owner")
        );
        assert_eq!(second.len(), 1);
        assert_eq!(
            second[0].status,
            AgentTaskLoopActionStatus::AlreadySatisfied
        );
        assert_eq!(record.pr_ownerships.len(), 1);
        assert_eq!(record.pr_ownerships[0].entity_id.as_deref(), Some("pr:42"));
    }

    #[test]
    fn pr_ownership_red_checks_increment_until_retry_limit() {
        let mut record = AgentTaskLoopControllerRecord::new("loop", "review", "v1");
        let request = AgentTaskPrOwnershipRequest {
            ownership_id: "run-123".to_string(),
            component_id: None,
            path: None,
            base: "main".to_string(),
            head: "fix/pr-owner".to_string(),
            pr_number: Some(42),
            pr_url: None,
            max_retries: 2,
            merge_required: false,
        };

        let first = record.record_pr_ownership_status(
            &request,
            Some("pr:42".to_string()),
            AgentTaskPrOwnershipStatusUpdate {
                pr_number: Some(42),
                ci_state: Some("terminal_failed".to_string()),
                retry_count: 1,
                ..AgentTaskPrOwnershipStatusUpdate::default()
            },
        );
        let second = record.record_pr_ownership_status(
            &request,
            Some("pr:42".to_string()),
            AgentTaskPrOwnershipStatusUpdate {
                pr_number: Some(42),
                ci_state: Some("terminal_failed".to_string()),
                retry_count: 2,
                ..AgentTaskPrOwnershipStatusUpdate::default()
            },
        );

        assert_eq!(first.state, AgentTaskPrOwnershipState::ChangesRequested);
        assert_eq!(second.state, AgentTaskPrOwnershipState::RetryLimitReached);
        assert_eq!(record.pr_ownerships.len(), 1);
        assert_eq!(record.pr_ownerships[0].retry_count, 2);
    }

    #[test]
    fn policy_transitions_can_match_structured_event_jsonpath() {
        let mut record = AgentTaskLoopControllerRecord::new("loop", "validate", "v1");
        let policy = AgentTaskLoopPolicy {
            policy_id: "validation-policy".to_string(),
            transitions: vec![AgentTaskLoopTransition {
                transition_id: "actionable-findings".to_string(),
                from_phase: Some("validate".to_string()),
                on_event_type: Some("validation.completed".to_string()),
                when_json_path: Some(
                    "$.event.payload.findings[?(@.actionable == true)]".to_string(),
                ),
                actions: vec![AgentTaskLoopPolicyAction::FanOut {
                    dedupe_key: "validation:run-1:actionable-findings".to_string(),
                    entity_ids: vec!["finding:a".to_string()],
                    request_template: json!({ "kind": "repair" }),
                }],
            }],
        };
        let actions = record.evaluate_policy(
            &policy,
            Some(&AgentTaskLoopExternalEvent {
                event_id: "event-1".to_string(),
                event_type: "validation.completed".to_string(),
                event_key: None,
                entity_id: None,
                payload: json!({
                    "findings": [
                        { "id": "a", "actionable": true },
                        { "id": "b", "actionable": false }
                    ]
                }),
            }),
        );

        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].status, AgentTaskLoopActionStatus::Pending);
    }

    #[test]
    fn runner_policy_prefers_declared_runner_when_available() {
        let record = AgentTaskLoopControllerRecord::new("loop", "dispatch", "v1");
        let action = AgentTaskLoopPolicyAction::SpawnTask {
            dedupe_key: "task:lab".to_string(),
            entity_id: None,
            request: json!({ "task": "repair", "runner": "homeboy-lab" }),
        };

        let decision = record.resolve_action_runner_policy(&action, |runner| {
            assert_eq!(runner, "homeboy-lab");
            AgentTaskLoopRunnerAvailability::Available
        });

        assert_eq!(
            decision.target,
            Some(AgentTaskLoopRunnerExecutionTarget::Runner(
                "homeboy-lab".to_string()
            ))
        );
        assert_eq!(decision.blocked_status, None);
        assert_eq!(decision.diagnostic, None);
    }

    #[test]
    fn runner_policy_allows_explicit_local_fallback_when_runner_is_unavailable() {
        let record = AgentTaskLoopControllerRecord::new("loop", "dispatch", "v1");
        let action = AgentTaskLoopPolicyAction::SpawnTask {
            dedupe_key: "task:lab".to_string(),
            entity_id: None,
            request: json!({
                "task": "repair",
                "runner": "homeboy-lab",
                "local_fallback": "allowed"
            }),
        };

        let decision = record.resolve_action_runner_policy(&action, |_| {
            AgentTaskLoopRunnerAvailability::Unavailable {
                reason: "runner heartbeat is stale".to_string(),
            }
        });

        assert_eq!(
            decision.target,
            Some(AgentTaskLoopRunnerExecutionTarget::Local)
        );
        assert_eq!(decision.blocked_status, None);
        assert_eq!(
            decision
                .diagnostic
                .as_ref()
                .map(|diagnostic| diagnostic.code.as_str()),
            Some("runner_unavailable_local_fallback_allowed")
        );
    }

    #[test]
    fn runner_policy_denies_local_fallback_for_unavailable_required_runner() {
        let record = AgentTaskLoopControllerRecord::new("loop", "dispatch", "v1");
        let action = AgentTaskLoopPolicyAction::SpawnTask {
            dedupe_key: "task:lab".to_string(),
            entity_id: None,
            request: json!({
                "task": "repair",
                "runner": "homeboy-lab",
                "local_fallback": "denied"
            }),
        };

        let decision = record.resolve_action_runner_policy(&action, |_| {
            AgentTaskLoopRunnerAvailability::Unavailable {
                reason: "runner is not registered".to_string(),
            }
        });

        assert_eq!(decision.target, None);
        assert_eq!(
            decision.blocked_status,
            Some(AgentTaskLoopActionStatus::BlockedRunnerUnavailable)
        );
        let diagnostic = decision.diagnostic.expect("blocked diagnostic");
        assert_eq!(diagnostic.code, "blocked_runner_unavailable");
        assert_eq!(diagnostic.runner.as_deref(), Some("homeboy-lab"));
    }

    #[test]
    fn runner_policy_blocks_remote_materialization_failures() {
        let record = AgentTaskLoopControllerRecord::new("loop", "dispatch", "v1");
        let action = AgentTaskLoopPolicyAction::FanOut {
            dedupe_key: "fanout:lab".to_string(),
            entity_ids: vec!["finding:1".to_string()],
            request_template: json!({
                "task": "repair",
                "runner": "homeboy-lab",
                "local_fallback": false
            }),
        };

        let decision = record.resolve_action_runner_policy(&action, |_| {
            AgentTaskLoopRunnerAvailability::MaterializationBlocked {
                reason: "workspace snapshot could not be materialized remotely".to_string(),
            }
        });

        assert_eq!(decision.target, None);
        assert_eq!(
            decision.blocked_status,
            Some(AgentTaskLoopActionStatus::BlockedRemoteMaterialization)
        );
        assert_eq!(
            decision
                .diagnostic
                .as_ref()
                .map(|diagnostic| diagnostic.code.as_str()),
            Some("blocked_remote_materialization")
        );
    }

    #[test]
    fn runner_policy_blocks_local_execution_when_fallback_is_denied_without_runner() {
        let record = AgentTaskLoopControllerRecord::new("loop", "dispatch", "v1");
        let action = AgentTaskLoopPolicyAction::RouteFinding {
            finding: AgentTaskLoopFindingPacket {
                finding_id: "finding-1".to_string(),
                severity: "high".to_string(),
                summary: "drift".to_string(),
                owner: None,
                source_transformer: None,
                reproduction_key: None,
                lineage: Vec::new(),
                payload: Value::Null,
            },
            dedupe_key: "finding:1".to_string(),
            entity_id: Some("finding:1".to_string()),
            request_template: json!({
                "task": "repair",
                "local_fallback": "denied"
            }),
        };

        let decision = record.resolve_action_runner_policy(&action, |_| {
            unreachable!("no runner should not probe runner availability")
        });

        assert_eq!(decision.target, None);
        assert_eq!(
            decision.blocked_status,
            Some(AgentTaskLoopActionStatus::BlockedLocalFallbackDenied)
        );
        assert_eq!(
            decision
                .diagnostic
                .as_ref()
                .map(|diagnostic| diagnostic.code.as_str()),
            Some("blocked_local_fallback_denied")
        );
    }

    #[test]
    fn runner_policy_block_persists_status_and_diagnostic() {
        let mut record = AgentTaskLoopControllerRecord::new("loop", "dispatch", "v1");
        let action = record.record_action(
            AgentTaskLoopPolicyAction::SpawnTask {
                dedupe_key: "task:lab".to_string(),
                entity_id: Some("finding:1".to_string()),
                request: json!({ "task": "repair", "runner": "homeboy-lab" }),
            },
            "policy matched",
        );

        let decision = record.resolve_action_runner_policy(&action.action, |_| {
            AgentTaskLoopRunnerAvailability::Unavailable {
                reason: "runner heartbeat is stale".to_string(),
            }
        });
        record
            .block_action_for_runner_policy(
                &action.action_id,
                decision.blocked_status.expect("blocked status"),
                decision.diagnostic.expect("blocked diagnostic"),
            )
            .expect("blocked action recorded");

        let persisted_action = record
            .next_actions
            .iter()
            .find(|candidate| candidate.action_id == action.action_id)
            .expect("action present");
        assert_eq!(
            persisted_action.status,
            AgentTaskLoopActionStatus::BlockedRunnerUnavailable
        );
        assert_eq!(persisted_action.diagnostics.len(), 1);
        assert_eq!(
            persisted_action.diagnostics[0].code,
            "blocked_runner_unavailable"
        );
        assert_eq!(record.history[0].event_type, "runner_policy.blocked");
    }

    #[test]
    fn finding_packets_route_once_with_lineage() {
        let mut record = AgentTaskLoopControllerRecord::new("loop", "validate", "v1");
        let finding = AgentTaskLoopFindingPacket {
            finding_id: "finding-1".to_string(),
            severity: "high".to_string(),
            summary: "layout drift".to_string(),
            owner: Some("transformer".to_string()),
            source_transformer: Some("hero".to_string()),
            reproduction_key: Some("page:/#hero".to_string()),
            lineage: vec![AgentTaskLoopArtifactRef {
                uri: "artifact://candidate/site".to_string(),
                kind: Some("static_site_candidate".to_string()),
                label: Some("candidate".to_string()),
            }],
            payload: json!({ "selector": ".hero" }),
        };

        let first = record.route_finding_packet(
            finding.clone(),
            json!({ "task": "iterate-transformer", "finding": finding }),
        );
        let second = record.route_finding_packet(
            AgentTaskLoopFindingPacket {
                finding_id: "finding-1b".to_string(),
                reproduction_key: Some("page:/#hero".to_string()),
                ..match first.action.clone() {
                    AgentTaskLoopPolicyAction::RouteFinding { finding, .. } => finding,
                    _ => unreachable!("route finding action"),
                }
            },
            json!({ "task": "iterate-transformer" }),
        );

        assert_eq!(first.status, AgentTaskLoopActionStatus::Pending);
        assert_eq!(second.status, AgentTaskLoopActionStatus::AlreadySatisfied);
        assert_eq!(first.dedupe_key.as_deref(), Some("finding:page:/#hero"));
        let entity = record.entities.get("finding:page___hero").expect("entity");
        assert_eq!(entity.state.as_deref(), Some("routed"));
        assert_eq!(entity.artifact_refs.len(), 1);
        assert_eq!(entity.provenance[0].uri, "artifact://candidate/site");
    }

    #[test]
    fn candidate_patch_validation_promotes_passes_to_human_ready() {
        let mut record = AgentTaskLoopControllerRecord::new("loop", "repair", "v1");
        let action = record.record_candidate_patch_validation(
            candidate_patch(1),
            AgentTaskLoopCandidateValidation {
                validation_id: "validation-1".to_string(),
                status: AgentTaskLoopCandidateValidationStatus::Passed,
                evidence: vec![artifact_ref("artifact://validation/report")],
                details: json!({ "passed": true }),
            },
            AgentTaskLoopCandidateLoopLimits { max_attempts: 2 },
        );

        assert_eq!(action.status, AgentTaskLoopActionStatus::Pending);
        assert_eq!(record.state, AgentTaskLoopControllerState::HumanReady);
        let entity = record
            .entities
            .get("candidate_patch:candidate-1")
            .expect("candidate entity");
        assert_eq!(entity.state.as_deref(), Some("validated"));
        assert!(entity.human_ready);
        assert_eq!(entity.artifact_refs.len(), 3);
    }

    #[test]
    fn candidate_patch_validation_marks_retry_limit_stop_condition() {
        let mut record = AgentTaskLoopControllerRecord::new("loop", "repair", "v1");
        record.record_candidate_patch_validation(
            candidate_patch(2),
            AgentTaskLoopCandidateValidation {
                validation_id: "validation-2".to_string(),
                status: AgentTaskLoopCandidateValidationStatus::Failed,
                evidence: vec![artifact_ref("artifact://validation/failure")],
                details: json!({ "passed": false }),
            },
            AgentTaskLoopCandidateLoopLimits { max_attempts: 2 },
        );

        assert_eq!(record.state, AgentTaskLoopControllerState::HumanReady);
        let entity = record
            .entities
            .get("candidate_patch:candidate-1")
            .expect("candidate entity");
        assert_eq!(entity.state.as_deref(), Some("retry_limit_reached"));
        assert!(entity.human_ready);
    }

    #[test]
    fn candidate_patch_validation_keeps_failed_candidate_retryable() {
        let mut record = AgentTaskLoopControllerRecord::new("loop", "repair", "v1");
        record.record_candidate_patch_validation(
            candidate_patch(1),
            AgentTaskLoopCandidateValidation {
                validation_id: "validation-1".to_string(),
                status: AgentTaskLoopCandidateValidationStatus::Failed,
                evidence: vec![artifact_ref("artifact://validation/failure")],
                details: json!({ "passed": false }),
            },
            AgentTaskLoopCandidateLoopLimits { max_attempts: 2 },
        );

        assert_eq!(record.state, AgentTaskLoopControllerState::Running);
        let entity = record
            .entities
            .get("candidate_patch:candidate-1")
            .expect("candidate entity");
        assert_eq!(entity.state.as_deref(), Some("needs_retry"));
        assert!(!entity.human_ready);
    }

    #[test]
    fn verify_commands_are_reusable_gate_bundle_checks() {
        let bundle = AgentTaskGateBundle::from_verify_commands(
            "candidate-gates",
            vec!["cargo test --lib".to_string()],
        );

        assert_eq!(bundle.bundle_id, "candidate-gates");
        assert_eq!(bundle.checks[0].kind, AgentTaskGateBundleCheckKind::Command);
        assert_eq!(bundle.checks[0].input["command"], json!("cargo test --lib"));
        assert!(bundle.checks[0].retryable);
    }

    fn candidate_patch(attempt: u32) -> AgentTaskLoopCandidatePatch {
        AgentTaskLoopCandidatePatch {
            candidate_id: "candidate-1".to_string(),
            patch: artifact_ref("artifact://patch/fix.diff"),
            finding_id: Some("finding-1".to_string()),
            worktree: Some("/tmp/homeboy-candidate".to_string()),
            attempt,
            lineage: vec![artifact_ref("artifact://finding/finding-1")],
        }
    }

    fn artifact_ref(uri: &str) -> AgentTaskLoopArtifactRef {
        AgentTaskLoopArtifactRef {
            uri: uri.to_string(),
            kind: Some("artifact".to_string()),
            label: None,
        }
    }
}
