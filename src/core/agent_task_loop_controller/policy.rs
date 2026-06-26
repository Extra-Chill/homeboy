use super::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;

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
    RunCommand {
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        dynamic_artifact: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        group_by: Vec<String>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        requires_non_empty: bool,
        #[serde(
            default = "default_fan_out_max_items",
            skip_serializing_if = "is_default_fan_out_max_items"
        )]
        max_items: usize,
        #[serde(default = "default_true", skip_serializing_if = "is_true")]
        fail_fast: bool,
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
