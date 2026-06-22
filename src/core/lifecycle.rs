//! Generic lifecycle contracts for fuzz-safe mutable workloads.
//!
//! Homeboy owns the vocabulary and result metadata shape. Product/runtime
//! implementations provide concrete hook execution through extensions.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::core::artifact_contract::ArtifactContract;

pub const LIFECYCLE_CONTRACT_SCHEMA: &str = "homeboy/lifecycle-contract/v1";
pub const LIFECYCLE_RESULT_SCHEMA: &str = "homeboy/lifecycle-result/v1";
pub const LIFECYCLE_SNAPSHOT_REF_SCHEMA: &str = "homeboy/lifecycle-snapshot-ref/v1";
pub const LIFECYCLE_CONTRACT_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LifecycleContract {
    #[serde(default = "lifecycle_contract_schema")]
    pub schema: String,
    #[serde(default = "lifecycle_contract_version")]
    pub version: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub phases: Vec<LifecyclePhaseContract>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LifecyclePhaseContract {
    pub id: String,
    pub phase: LifecyclePhaseKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extension_hook: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required: Option<bool>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LifecyclePhaseKind {
    Prepare,
    Seed,
    Snapshot,
    Reset,
    Rollback,
    Teardown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LifecycleResultMetadata {
    #[serde(default = "lifecycle_result_schema")]
    pub schema: String,
    #[serde(default = "lifecycle_contract_version")]
    pub version: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub phases: Vec<LifecyclePhaseResult>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub snapshot_refs: Vec<LifecycleSnapshotRef>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LifecyclePhaseResult {
    pub id: String,
    pub phase: LifecyclePhaseKind,
    pub status: LifecyclePhaseStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LifecyclePhaseStatus {
    Passed,
    Failed,
    Skipped,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LifecycleSnapshotRef {
    #[serde(default = "lifecycle_snapshot_ref_schema")]
    pub schema: String,
    pub id: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact: Option<ArtifactContract>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locator: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, String>,
}

fn lifecycle_contract_schema() -> String {
    LIFECYCLE_CONTRACT_SCHEMA.to_string()
}

fn lifecycle_result_schema() -> String {
    LIFECYCLE_RESULT_SCHEMA.to_string()
}

fn lifecycle_snapshot_ref_schema() -> String {
    LIFECYCLE_SNAPSHOT_REF_SCHEMA.to_string()
}

fn lifecycle_contract_version() -> u32 {
    LIFECYCLE_CONTRACT_VERSION
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn lifecycle_contract_covers_fuzz_safe_phases() {
        let contract: LifecycleContract = serde_json::from_value(json!({
            "phases": [
                { "id": "prepare", "phase": "prepare", "extension_hook": "runtime.prepare" },
                { "id": "seed", "phase": "seed", "extension_hook": "runtime.seed" },
                { "id": "snapshot", "phase": "snapshot", "extension_hook": "runtime.snapshot" },
                { "id": "reset", "phase": "reset", "extension_hook": "runtime.reset" },
                { "id": "rollback", "phase": "rollback", "extension_hook": "runtime.rollback" },
                { "id": "teardown", "phase": "teardown", "extension_hook": "runtime.teardown" }
            ]
        }))
        .expect("lifecycle contract");

        assert_eq!(contract.schema, LIFECYCLE_CONTRACT_SCHEMA);
        assert_eq!(contract.version, LIFECYCLE_CONTRACT_VERSION);
        assert_eq!(contract.phases.len(), 6);
        assert_eq!(contract.phases[2].phase, LifecyclePhaseKind::Snapshot);
    }

    #[test]
    fn lifecycle_result_metadata_serializes_snapshot_refs() {
        let result = LifecycleResultMetadata {
            schema: LIFECYCLE_RESULT_SCHEMA.to_string(),
            version: LIFECYCLE_CONTRACT_VERSION,
            phases: vec![LifecyclePhaseResult {
                id: "snapshot".to_string(),
                phase: LifecyclePhaseKind::Snapshot,
                status: LifecyclePhaseStatus::Passed,
                snapshot_ref: Some("snapshot-1".to_string()),
                started_at: None,
                finished_at: None,
                message: None,
            }],
            snapshot_refs: vec![LifecycleSnapshotRef {
                schema: LIFECYCLE_SNAPSHOT_REF_SCHEMA.to_string(),
                id: "snapshot-1".to_string(),
                kind: "database".to_string(),
                phase_id: Some("snapshot".to_string()),
                artifact_id: Some("db-snapshot".to_string()),
                artifact: None,
                locator: None,
                created_at: None,
                metadata: BTreeMap::new(),
            }],
            metadata: BTreeMap::new(),
        };

        let value = serde_json::to_value(result).expect("result json");

        assert_eq!(value["schema"], LIFECYCLE_RESULT_SCHEMA);
        assert_eq!(value["phases"][0]["snapshot_ref"], "snapshot-1");
        assert_eq!(
            value["snapshot_refs"][0]["schema"],
            LIFECYCLE_SNAPSHOT_REF_SCHEMA
        );
    }
}
