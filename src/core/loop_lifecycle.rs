//! Generic loop lifecycle schema records.
//!
//! These records are intentionally product-neutral: core owns stable schema
//! names and lifecycle vocabulary, while producers attach domain details through
//! metadata or flattened extension fields.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::core::artifact_contract::ArtifactContract;

pub const LOOP_RUN_SCHEMA: &str = "homeboy/loop-run/v1";
pub const LOOP_ITERATION_SCHEMA: &str = "homeboy/loop-iteration/v1";
pub const LOOP_EVIDENCE_SCHEMA: &str = "homeboy/loop-evidence/v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LoopRunRecord {
    #[serde(default = "loop_run_schema")]
    pub schema: String,
    pub id: String,
    pub status: LoopLifecycleStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub controller: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub iteration_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LoopIterationRecord {
    #[serde(default = "loop_iteration_schema")]
    pub schema: String,
    pub id: String,
    pub run_id: String,
    pub index: u32,
    pub status: LoopLifecycleStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LoopEvidenceRecord {
    #[serde(default = "loop_evidence_schema")]
    pub schema: String,
    pub id: String,
    pub run_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iteration_id: Option<String>,
    pub kind: String,
    pub status: LoopEvidenceStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact: Option<ArtifactContract>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LoopLifecycleStatus {
    Queued,
    Running,
    Succeeded,
    Failed,
    Cancelled,
    TimedOut,
    Blocked,
    Skipped,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LoopEvidenceStatus {
    Captured,
    Missing,
    Failed,
    Superseded,
}

fn loop_run_schema() -> String {
    LOOP_RUN_SCHEMA.to_string()
}

fn loop_iteration_schema() -> String {
    LOOP_ITERATION_SCHEMA.to_string()
}

fn loop_evidence_schema() -> String {
    LOOP_EVIDENCE_SCHEMA.to_string()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn loop_run_record_serializes_golden_json() {
        let record = LoopRunRecord {
            schema: LOOP_RUN_SCHEMA.to_string(),
            id: "loop-run-1".to_string(),
            status: LoopLifecycleStatus::Running,
            controller: Some("deterministic-loop".to_string()),
            started_at: Some("2026-06-30T15:00:00Z".to_string()),
            finished_at: None,
            updated_at: Some("2026-06-30T15:01:00Z".to_string()),
            iteration_ids: vec!["iteration-1".to_string()],
            evidence_ids: vec!["evidence-1".to_string()],
            metadata: Value::Null,
            extra: BTreeMap::new(),
        };

        let golden = json!({
            "schema": LOOP_RUN_SCHEMA,
            "id": "loop-run-1",
            "status": "running",
            "controller": "deterministic-loop",
            "started_at": "2026-06-30T15:00:00Z",
            "updated_at": "2026-06-30T15:01:00Z",
            "iteration_ids": ["iteration-1"],
            "evidence_ids": ["evidence-1"]
        });

        assert_eq!(serde_json::to_value(&record).expect("serialize"), golden);
        assert_eq!(
            serde_json::from_value::<LoopRunRecord>(golden).expect("deserialize"),
            record
        );
    }

    #[test]
    fn loop_iteration_record_serializes_golden_json() {
        let record = LoopIterationRecord {
            schema: LOOP_ITERATION_SCHEMA.to_string(),
            id: "iteration-1".to_string(),
            run_id: "loop-run-1".to_string(),
            index: 1,
            status: LoopLifecycleStatus::Succeeded,
            started_at: Some("2026-06-30T15:01:00Z".to_string()),
            finished_at: Some("2026-06-30T15:02:00Z".to_string()),
            summary: Some("First pass completed".to_string()),
            evidence_ids: vec!["evidence-1".to_string()],
            metadata: json!({ "attempt": 1 }),
            extra: BTreeMap::new(),
        };

        let golden = json!({
            "schema": LOOP_ITERATION_SCHEMA,
            "id": "iteration-1",
            "run_id": "loop-run-1",
            "index": 1,
            "status": "succeeded",
            "started_at": "2026-06-30T15:01:00Z",
            "finished_at": "2026-06-30T15:02:00Z",
            "summary": "First pass completed",
            "evidence_ids": ["evidence-1"],
            "metadata": { "attempt": 1 }
        });

        assert_eq!(serde_json::to_value(&record).expect("serialize"), golden);
        assert_eq!(
            serde_json::from_value::<LoopIterationRecord>(golden).expect("deserialize"),
            record
        );
    }

    #[test]
    fn loop_evidence_record_serializes_golden_json() {
        let record = LoopEvidenceRecord {
            schema: LOOP_EVIDENCE_SCHEMA.to_string(),
            id: "evidence-1".to_string(),
            run_id: "loop-run-1".to_string(),
            iteration_id: Some("iteration-1".to_string()),
            kind: "review-proof".to_string(),
            status: LoopEvidenceStatus::Captured,
            target: Some("https://example.test/proof".to_string()),
            artifact: None,
            summary: Some("Reviewer-visible proof captured".to_string()),
            created_at: Some("2026-06-30T15:02:00Z".to_string()),
            metadata: Value::Null,
            extra: BTreeMap::new(),
        };

        let golden = json!({
            "schema": LOOP_EVIDENCE_SCHEMA,
            "id": "evidence-1",
            "run_id": "loop-run-1",
            "iteration_id": "iteration-1",
            "kind": "review-proof",
            "status": "captured",
            "target": "https://example.test/proof",
            "summary": "Reviewer-visible proof captured",
            "created_at": "2026-06-30T15:02:00Z"
        });

        assert_eq!(serde_json::to_value(&record).expect("serialize"), golden);
        assert_eq!(
            serde_json::from_value::<LoopEvidenceRecord>(golden).expect("deserialize"),
            record
        );
    }

    #[test]
    fn loop_records_default_schema_constants() {
        let run: LoopRunRecord = serde_json::from_value(json!({
            "id": "loop-run-1",
            "status": "queued"
        }))
        .expect("run");
        let iteration: LoopIterationRecord = serde_json::from_value(json!({
            "id": "iteration-1",
            "run_id": "loop-run-1",
            "index": 1,
            "status": "queued"
        }))
        .expect("iteration");
        let evidence: LoopEvidenceRecord = serde_json::from_value(json!({
            "id": "evidence-1",
            "run_id": "loop-run-1",
            "kind": "log",
            "status": "missing"
        }))
        .expect("evidence");

        assert_eq!(run.schema, LOOP_RUN_SCHEMA);
        assert_eq!(iteration.schema, LOOP_ITERATION_SCHEMA);
        assert_eq!(evidence.schema, LOOP_EVIDENCE_SCHEMA);
    }
}
