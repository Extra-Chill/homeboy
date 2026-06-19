use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const DETERMINISTIC_LOOP_PLAN_SCHEMA: &str = "homeboy/deterministic-loop-plan/v1";
pub const DETERMINISTIC_LOOP_RUN_IDENTITY_SCHEMA: &str =
    "homeboy/deterministic-loop-run-identity/v1";
pub const DETERMINISTIC_FANOUT_PLAN_SCHEMA: &str = "homeboy/deterministic-fanout-plan/v1";
pub const DETERMINISTIC_FANOUT_RESULT_SCHEMA: &str = "homeboy/deterministic-fanout-result/v1";
pub const DETERMINISTIC_EVIDENCE_SNAPSHOT_SCHEMA: &str =
    "homeboy/deterministic-evidence-snapshot/v1";

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DeterministicLoopStatus {
    #[default]
    Planned,
    Running,
    Waiting,
    Succeeded,
    Failed,
    Canceled,
    Skipped,
    Blocked,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeterministicLoopRunIdentity {
    #[serde(default = "run_identity_schema")]
    pub schema: String,
    pub loop_id: String,
    pub run_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_run_id: Option<String>,
    #[serde(default)]
    pub attempt: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DeterministicLoopPlan {
    #[serde(default = "loop_plan_schema")]
    pub schema: String,
    pub identity: DeterministicLoopRunIdentity,
    #[serde(default)]
    pub status: DeterministicLoopStatus,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fanouts: Vec<DeterministicFanoutPlan>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DeterministicFanoutPlan {
    #[serde(default = "fanout_plan_schema")]
    pub schema: String,
    pub fanout_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loop_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub records: Vec<DeterministicFanoutPlanRecord>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DeterministicFanoutPlanRecord {
    pub record_id: String,
    pub dedupe_key: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub input: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DeterministicFanoutResultRecord {
    #[serde(default = "fanout_result_schema")]
    pub schema: String,
    pub fanout_id: String,
    pub record_id: String,
    pub status: DeterministicLoopStatus,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<DeterministicEvidenceRef>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub output: Value,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub diagnostics: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DeterministicEvidenceSnapshot {
    #[serde(default = "evidence_snapshot_schema")]
    pub schema: String,
    pub snapshot_id: String,
    pub identity: DeterministicLoopRunIdentity,
    pub status: DeterministicLoopStatus,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fanout_results: Vec<DeterministicFanoutResultRecord>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<DeterministicEvidenceRef>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeterministicEvidenceRef {
    pub uri: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

impl DeterministicLoopRunIdentity {
    pub fn new(loop_id: impl Into<String>, run_id: impl Into<String>) -> Self {
        Self {
            schema: DETERMINISTIC_LOOP_RUN_IDENTITY_SCHEMA.to_string(),
            loop_id: loop_id.into(),
            run_id: run_id.into(),
            parent_run_id: None,
            attempt: 0,
            seed: None,
        }
    }
}

impl DeterministicLoopPlan {
    pub fn new(identity: DeterministicLoopRunIdentity) -> Self {
        Self {
            schema: DETERMINISTIC_LOOP_PLAN_SCHEMA.to_string(),
            identity,
            status: DeterministicLoopStatus::Planned,
            fanouts: Vec::new(),
            metadata: Value::Null,
        }
    }
}

impl DeterministicFanoutPlan {
    pub fn new(fanout_id: impl Into<String>) -> Self {
        Self {
            schema: DETERMINISTIC_FANOUT_PLAN_SCHEMA.to_string(),
            fanout_id: fanout_id.into(),
            loop_id: None,
            records: Vec::new(),
            metadata: Value::Null,
        }
    }
}

impl DeterministicFanoutResultRecord {
    pub fn new(
        fanout_id: impl Into<String>,
        record_id: impl Into<String>,
        status: DeterministicLoopStatus,
    ) -> Self {
        Self {
            schema: DETERMINISTIC_FANOUT_RESULT_SCHEMA.to_string(),
            fanout_id: fanout_id.into(),
            record_id: record_id.into(),
            status,
            evidence: Vec::new(),
            output: Value::Null,
            diagnostics: Value::Null,
        }
    }
}

impl DeterministicEvidenceSnapshot {
    pub fn new(
        snapshot_id: impl Into<String>,
        identity: DeterministicLoopRunIdentity,
        status: DeterministicLoopStatus,
    ) -> Self {
        Self {
            schema: DETERMINISTIC_EVIDENCE_SNAPSHOT_SCHEMA.to_string(),
            snapshot_id: snapshot_id.into(),
            identity,
            status,
            fanout_results: Vec::new(),
            evidence: Vec::new(),
            metadata: Value::Null,
        }
    }
}

fn loop_plan_schema() -> String {
    DETERMINISTIC_LOOP_PLAN_SCHEMA.to_string()
}

fn run_identity_schema() -> String {
    DETERMINISTIC_LOOP_RUN_IDENTITY_SCHEMA.to_string()
}

fn fanout_plan_schema() -> String {
    DETERMINISTIC_FANOUT_PLAN_SCHEMA.to_string()
}

fn fanout_result_schema() -> String {
    DETERMINISTIC_FANOUT_RESULT_SCHEMA.to_string()
}

fn evidence_snapshot_schema() -> String {
    DETERMINISTIC_EVIDENCE_SNAPSHOT_SCHEMA.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_loop_contract_serializes_stable_schema_and_status_values() {
        let identity = DeterministicLoopRunIdentity::new("loop-a", "run-a");
        let mut fanout = DeterministicFanoutPlan::new("fanout-a");
        fanout.loop_id = Some(identity.loop_id.clone());
        fanout.records.push(DeterministicFanoutPlanRecord {
            record_id: "record-a".to_string(),
            dedupe_key: "loop-a:record-a".to_string(),
            depends_on: Vec::new(),
            input: Value::Null,
        });

        let mut plan = DeterministicLoopPlan::new(identity.clone());
        plan.fanouts.push(fanout);

        let value = serde_json::to_value(&plan).expect("loop plan serializes");
        assert_eq!(value["schema"], DETERMINISTIC_LOOP_PLAN_SCHEMA);
        assert_eq!(
            value["identity"]["schema"],
            DETERMINISTIC_LOOP_RUN_IDENTITY_SCHEMA
        );
        assert_eq!(value["status"], "planned");
        assert_eq!(
            value["fanouts"][0]["schema"],
            DETERMINISTIC_FANOUT_PLAN_SCHEMA
        );

        let snapshot = DeterministicEvidenceSnapshot::new(
            "snapshot-a",
            identity,
            DeterministicLoopStatus::Succeeded,
        );
        let snapshot_value = serde_json::to_value(&snapshot).expect("snapshot serializes");
        assert_eq!(
            snapshot_value["schema"],
            DETERMINISTIC_EVIDENCE_SNAPSHOT_SCHEMA
        );
        assert_eq!(snapshot_value["status"], "succeeded");
    }
}
