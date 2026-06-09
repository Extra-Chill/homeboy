use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs;
use std::io::ErrorKind;
use std::path::PathBuf;

use crate::core::{paths, Error, Result};

pub const AGENT_TASK_LOOP_CONTROLLER_SCHEMA: &str = "homeboy/agent-task-loop-controller/v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskLoopControllerRecord {
    #[serde(default = "controller_schema")]
    pub schema: String,
    pub loop_id: String,
    pub phase: String,
    pub state: AgentTaskLoopControllerState,
    pub config_version: String,
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
    pub waits: Vec<AgentTaskLoopWait>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub feedback: Vec<AgentTaskLoopFeedbackArtifact>,
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
    WaitForEvent(AgentTaskLoopWait),
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
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskLoopActionStatus {
    Pending,
    AlreadySatisfied,
    Completed,
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
    pub fn from_verify_commands(bundle_id: impl Into<String>, commands: Vec<String>) -> Self {
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
            created_at: now.clone(),
            updated_at: now,
            entities: BTreeMap::new(),
            dedupe_keys: BTreeMap::new(),
            task_lineage: Vec::new(),
            gate_bundles: Vec::new(),
            gate_results: Vec::new(),
            waits: Vec::new(),
            feedback: Vec::new(),
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

        self.apply_action_side_effects(&action, status);
        let record = AgentTaskLoopPolicyActionRecord {
            action_id: format!("action-{}", self.next_actions.len() + 1),
            action,
            status,
            reason,
            created_at: now_timestamp(),
            dedupe_key,
        };
        self.next_actions.push(record.clone());
        self.touch();
        record
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

    fn apply_action_side_effects(
        &mut self,
        action: &AgentTaskLoopPolicyAction,
        status: AgentTaskLoopActionStatus,
    ) {
        if status == AgentTaskLoopActionStatus::AlreadySatisfied {
            return;
        }
        match action {
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
            AgentTaskLoopPolicyAction::MarkHumanReady { entity_id, reason } => {
                let _ = self.mark_human_ready(entity_id, reason.clone());
            }
            AgentTaskLoopPolicyAction::Complete { .. } => {
                self.state = AgentTaskLoopControllerState::Completed;
            }
            AgentTaskLoopPolicyAction::Abandon { .. } => {
                self.state = AgentTaskLoopControllerState::Abandoned;
            }
            AgentTaskLoopPolicyAction::Escalate { .. } => {
                self.state = AgentTaskLoopControllerState::Escalated;
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
        | AgentTaskLoopPolicyAction::FanOut { dedupe_key, .. } => Some(dedupe_key.clone()),
        AgentTaskLoopPolicyAction::WaitForEvent(wait) => Some(format!("wait:{}", wait.wait_key)),
        AgentTaskLoopPolicyAction::RunGates {
            bundle_id,
            entity_id,
        } => entity_id
            .as_ref()
            .map(|entity_id| format!("gate:{bundle_id}:{entity_id}")),
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

fn action_entity_id(action: &AgentTaskLoopPolicyAction) -> Option<String> {
    match action {
        AgentTaskLoopPolicyAction::SpawnTask { entity_id, .. }
        | AgentTaskLoopPolicyAction::RunGates { entity_id, .. } => entity_id.clone(),
        AgentTaskLoopPolicyAction::MarkHumanReady { entity_id, .. } => Some(entity_id.clone()),
        _ => None,
    }
}

fn action_name(action: &AgentTaskLoopPolicyAction) -> &'static str {
    match action {
        AgentTaskLoopPolicyAction::SpawnTask { .. } => "spawn_task",
        AgentTaskLoopPolicyAction::FanOut { .. } => "fan_out",
        AgentTaskLoopPolicyAction::Join { .. } => "join",
        AgentTaskLoopPolicyAction::Retry { .. } => "retry",
        AgentTaskLoopPolicyAction::RequestChanges { .. } => "request_changes",
        AgentTaskLoopPolicyAction::RunGates { .. } => "run_gates",
        AgentTaskLoopPolicyAction::WaitForEvent(_) => "wait_for_event",
        AgentTaskLoopPolicyAction::MarkHumanReady { .. } => "mark_human_ready",
        AgentTaskLoopPolicyAction::Complete { .. } => "complete",
        AgentTaskLoopPolicyAction::Abandon { .. } => "abandon",
        AgentTaskLoopPolicyAction::Escalate { .. } => "escalate",
    }
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
}
