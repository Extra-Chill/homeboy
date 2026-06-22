//! Product-neutral fuzzing contracts shared by runners, labs, and reports.
//!
//! These types define Homeboy-owned envelope shapes only. Product-specific
//! runners can attach their own details through `metadata` or flattened extras.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::core::artifact_contract::ArtifactContract;
use crate::core::{Error, Result};

pub const FUZZ_CORE_CONTRACT_SCHEMA: &str = "homeboy/fuzz-core-contract/v1";
pub const FUZZ_CONTRACT_VERSION: u32 = 1;
pub const FUZZ_SURFACE_SCHEMA: &str = "homeboy/fuzz-surface/v1";
pub const FUZZ_TARGET_SCHEMA: &str = "homeboy/fuzz-target/v1";
pub const FUZZ_WORKLOAD_SCHEMA: &str = "homeboy/fuzz-workload/v1";
pub const FUZZ_CAMPAIGN_SCHEMA: &str = "homeboy/fuzz-campaign/v1";
pub const FUZZ_CASE_SCHEMA: &str = "homeboy/fuzz-case/v1";
pub const FUZZ_SEED_SCHEMA: &str = "homeboy/fuzz-seed/v1";
pub const FUZZ_COVERAGE_SCHEMA: &str = "homeboy/fuzz-coverage/v1";
pub const FUZZ_FINDING_SCHEMA: &str = "homeboy/fuzz-finding/v1";
pub const FUZZ_ARTIFACT_SCHEMA: &str = "homeboy/fuzz-artifact/v1";
pub const FUZZ_THRESHOLD_SCHEMA: &str = "homeboy/fuzz-threshold/v1";
pub const FUZZ_PROVENANCE_SCHEMA: &str = "homeboy/fuzz-provenance/v1";
pub const FUZZ_REPLAY_SCHEMA: &str = "homeboy/fuzz-replay/v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FuzzCoreContract {
    #[serde(default = "fuzz_core_contract_schema")]
    pub schema: String,
    #[serde(default = "fuzz_contract_version")]
    pub version: u32,
    pub schemas: FuzzContractSchemas,
    pub safety_classes: Vec<FuzzSafetyClass>,
    pub finding_statuses: Vec<FuzzFindingStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FuzzContractSchemas {
    pub surface: String,
    pub target: String,
    pub workload: String,
    pub campaign: String,
    pub case: String,
    pub seed: String,
    pub coverage: String,
    pub finding: String,
    pub artifact: String,
    pub threshold: String,
    pub provenance: String,
    pub replay: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FuzzSafetyClass {
    ReadOnly,
    Idempotent,
    IsolatedMutation,
    Destructive,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FuzzSurface {
    #[serde(default = "fuzz_surface_schema")]
    pub schema: String,
    pub id: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    pub safety_class: FuzzSafetyClass,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub operations: Vec<FuzzOperation>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<FuzzInput>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

impl FuzzSurface {
    pub fn from_value(value: Value) -> std::result::Result<Self, String> {
        let mut surface: Self = serde_json::from_value(value).map_err(|err| err.to_string())?;
        surface.normalize()?;
        Ok(surface)
    }

    fn normalize(&mut self) -> std::result::Result<(), String> {
        self.schema = trim_or_default(&self.schema, FUZZ_SURFACE_SCHEMA);
        require_schema(&self.schema, FUZZ_SURFACE_SCHEMA, "fuzz surface")?;
        self.id = required_trimmed("id", &self.id)?;
        self.kind = required_trimmed("kind", &self.kind)?;
        self.label = normalize_optional_string(self.label.take());
        self.target = normalize_optional_string(self.target.take());
        for operation in &mut self.operations {
            operation.normalize()?;
        }
        for input in &mut self.inputs {
            input.normalize()?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FuzzOperation {
    pub id: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
}

impl FuzzOperation {
    fn normalize(&mut self) -> std::result::Result<(), String> {
        self.id = required_trimmed("operation.id", &self.id)?;
        self.kind = required_trimmed("operation.kind", &self.kind)?;
        self.target_id = normalize_optional_string(self.target_id.take());
        self.label = normalize_optional_string(self.label.take());
        self.tags = normalize_string_vec(std::mem::take(&mut self.tags));
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FuzzTarget {
    #[serde(default = "fuzz_target_schema")]
    pub schema: String,
    pub id: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locator: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub operations: Vec<FuzzOperation>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FuzzInput {
    pub name: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generator: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub constraints: Vec<String>,
}

impl FuzzInput {
    fn normalize(&mut self) -> std::result::Result<(), String> {
        self.name = required_trimmed("input.name", &self.name)?;
        self.kind = required_trimmed("input.kind", &self.kind)?;
        self.generator = normalize_optional_string(self.generator.take());
        self.constraints = normalize_string_vec(std::mem::take(&mut self.constraints));
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FuzzWorkload {
    #[serde(default = "fuzz_workload_schema")]
    pub schema: String,
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub safety_class: FuzzSafetyClass,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub surface_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub operations: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub seed_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub case_budget: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_budget_seconds: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub thresholds: Vec<FuzzThreshold>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FuzzCampaign {
    #[serde(default = "fuzz_campaign_schema")]
    pub schema: String,
    #[serde(default = "fuzz_contract_version")]
    pub version: u32,
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub safety_class: FuzzSafetyClass,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub surfaces: Vec<FuzzSurface>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub targets: Vec<FuzzTarget>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workloads: Vec<FuzzWorkload>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cases: Vec<FuzzCase>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub seeds: Vec<FuzzSeed>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub coverage: Vec<FuzzCoverage>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub findings: Vec<FuzzFinding>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<FuzzArtifact>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub thresholds: Vec<FuzzThreshold>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<FuzzProvenance>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replay: Option<FuzzReplayMetadata>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FuzzCase {
    #[serde(default = "fuzz_case_schema")]
    pub schema: String,
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workload_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replay_id: Option<String>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub input: Value,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub expected: Value,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub observed: Value,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FuzzSeed {
    #[serde(default = "fuzz_seed_schema")]
    pub schema: String,
    pub id: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact: Option<ArtifactContract>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FuzzCoverage {
    #[serde(default = "fuzz_coverage_schema")]
    pub schema: String,
    pub id: String,
    pub metric: String,
    pub covered: u64,
    pub total: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ratio: Option<f64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gaps: Vec<FuzzCoverageGap>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FuzzCoverageGap {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub surface_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FuzzFinding {
    #[serde(default = "fuzz_finding_schema")]
    pub schema: String,
    pub id: String,
    pub title: String,
    pub severity: String,
    pub status: FuzzFindingStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub surface_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub case_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workload_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FuzzFindingStatus {
    Open,
    Confirmed,
    Mitigated,
    Suppressed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FuzzArtifact {
    #[serde(default = "fuzz_artifact_schema")]
    pub schema: String,
    pub id: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact: Option<ArtifactContract>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FuzzThreshold {
    #[serde(default = "fuzz_threshold_schema")]
    pub schema: String,
    pub id: String,
    pub metric: String,
    pub operator: FuzzThresholdOperator,
    pub value: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub safety_class: Option<FuzzSafetyClass>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FuzzThresholdOperator {
    GreaterThan,
    GreaterThanOrEqual,
    LessThan,
    LessThanOrEqual,
    Equal,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FuzzProvenance {
    #[serde(default = "fuzz_provenance_schema")]
    pub schema: String,
    pub producer: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub producer_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invocation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FuzzReplayMetadata {
    #[serde(default = "fuzz_replay_schema")]
    pub schema: String,
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_id: Option<String>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

pub fn fuzz_core_contract() -> FuzzCoreContract {
    FuzzCoreContract {
        schema: FUZZ_CORE_CONTRACT_SCHEMA.to_string(),
        version: FUZZ_CONTRACT_VERSION,
        schemas: FuzzContractSchemas {
            surface: FUZZ_SURFACE_SCHEMA.to_string(),
            target: FUZZ_TARGET_SCHEMA.to_string(),
            workload: FUZZ_WORKLOAD_SCHEMA.to_string(),
            campaign: FUZZ_CAMPAIGN_SCHEMA.to_string(),
            case: FUZZ_CASE_SCHEMA.to_string(),
            seed: FUZZ_SEED_SCHEMA.to_string(),
            coverage: FUZZ_COVERAGE_SCHEMA.to_string(),
            finding: FUZZ_FINDING_SCHEMA.to_string(),
            artifact: FUZZ_ARTIFACT_SCHEMA.to_string(),
            threshold: FUZZ_THRESHOLD_SCHEMA.to_string(),
            provenance: FUZZ_PROVENANCE_SCHEMA.to_string(),
            replay: FUZZ_REPLAY_SCHEMA.to_string(),
        },
        safety_classes: vec![
            FuzzSafetyClass::ReadOnly,
            FuzzSafetyClass::Idempotent,
            FuzzSafetyClass::IsolatedMutation,
            FuzzSafetyClass::Destructive,
        ],
        finding_statuses: vec![
            FuzzFindingStatus::Open,
            FuzzFindingStatus::Confirmed,
            FuzzFindingStatus::Mitigated,
            FuzzFindingStatus::Suppressed,
        ],
    }
}

pub fn parse_fuzz_results_file(path: &Path) -> Result<FuzzCampaign> {
    let contents = std::fs::read_to_string(path)
        .map_err(|err| Error::internal_io(err.to_string(), Some(path.display().to_string())))?;
    let campaign: FuzzCampaign = serde_json::from_str(&contents).map_err(|err| {
        Error::validation_invalid_json(
            err,
            Some(format!("parse fuzz results file {}", path.display())),
            Some(contents),
        )
    })?;
    if campaign.schema != FUZZ_CAMPAIGN_SCHEMA {
        return Err(Error::validation_invalid_argument(
            "schema",
            format!(
                "fuzz results schema must be {FUZZ_CAMPAIGN_SCHEMA}, got {}",
                campaign.schema
            ),
            Some(campaign.schema),
            None,
        ));
    }
    Ok(campaign)
}

fn fuzz_core_contract_schema() -> String {
    FUZZ_CORE_CONTRACT_SCHEMA.to_string()
}

fn fuzz_contract_version() -> u32 {
    FUZZ_CONTRACT_VERSION
}

fn fuzz_surface_schema() -> String {
    FUZZ_SURFACE_SCHEMA.to_string()
}

fn fuzz_target_schema() -> String {
    FUZZ_TARGET_SCHEMA.to_string()
}

fn fuzz_workload_schema() -> String {
    FUZZ_WORKLOAD_SCHEMA.to_string()
}

fn fuzz_campaign_schema() -> String {
    FUZZ_CAMPAIGN_SCHEMA.to_string()
}

fn fuzz_case_schema() -> String {
    FUZZ_CASE_SCHEMA.to_string()
}

fn fuzz_seed_schema() -> String {
    FUZZ_SEED_SCHEMA.to_string()
}

fn fuzz_coverage_schema() -> String {
    FUZZ_COVERAGE_SCHEMA.to_string()
}

fn fuzz_finding_schema() -> String {
    FUZZ_FINDING_SCHEMA.to_string()
}

fn fuzz_artifact_schema() -> String {
    FUZZ_ARTIFACT_SCHEMA.to_string()
}

fn fuzz_threshold_schema() -> String {
    FUZZ_THRESHOLD_SCHEMA.to_string()
}

fn fuzz_provenance_schema() -> String {
    FUZZ_PROVENANCE_SCHEMA.to_string()
}

fn fuzz_replay_schema() -> String {
    FUZZ_REPLAY_SCHEMA.to_string()
}

fn normalize_optional_string(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    })
}

fn normalize_string_vec(values: Vec<String>) -> Vec<String> {
    values
        .into_iter()
        .filter_map(|value| {
            let trimmed = value.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_string())
        })
        .collect()
}

fn trim_or_default(value: &str, default: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        default.to_string()
    } else {
        trimmed.to_string()
    }
}

fn required_trimmed(field: &str, value: &str) -> std::result::Result<String, String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        Err(format!("{field} must be non-empty"))
    } else {
        Ok(trimmed.to_string())
    }
}

fn require_schema(actual: &str, expected: &str, label: &str) -> std::result::Result<(), String> {
    if actual == expected {
        Ok(())
    } else {
        Err(format!("{label} schema must be {expected}"))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn core_contract_lists_product_neutral_schema_ids() {
        let contract = fuzz_core_contract();

        assert_eq!(contract.schema, FUZZ_CORE_CONTRACT_SCHEMA);
        assert_eq!(contract.version, FUZZ_CONTRACT_VERSION);
        assert_eq!(contract.schemas.surface, FUZZ_SURFACE_SCHEMA);
        assert_eq!(contract.schemas.target, FUZZ_TARGET_SCHEMA);
        assert_eq!(contract.schemas.campaign, FUZZ_CAMPAIGN_SCHEMA);
        assert_eq!(contract.schemas.case, FUZZ_CASE_SCHEMA);
        assert_eq!(contract.schemas.replay, FUZZ_REPLAY_SCHEMA);
        assert!(contract
            .safety_classes
            .contains(&FuzzSafetyClass::IsolatedMutation));
        assert!(contract.finding_statuses.contains(&FuzzFindingStatus::Open));
    }

    #[test]
    fn surface_normalizes_optional_fields_and_nested_inputs() {
        let surface = FuzzSurface::from_value(json!({
            "id": " orders ",
            "kind": " api ",
            "label": " ",
            "target": " https://example.test/resource ",
            "safety_class": "read_only",
            "operations": [
                { "id": " list ", "kind": " read ", "tags": [" stable ", " "] }
            ],
            "inputs": [
                { "name": " query ", "kind": " string ", "generator": " ascii ", "constraints": [" max:64 "] }
            ],
            "owner": "extension"
        }))
        .expect("surface contract");

        assert_eq!(surface.schema, FUZZ_SURFACE_SCHEMA);
        assert_eq!(surface.id, "orders");
        assert_eq!(surface.kind, "api");
        assert_eq!(surface.label, None);
        assert_eq!(
            surface.target.as_deref(),
            Some("https://example.test/resource")
        );
        assert_eq!(surface.operations[0].tags, vec!["stable"]);
        assert_eq!(surface.inputs[0].constraints, vec!["max:64"]);
        assert_eq!(surface.extra["owner"], "extension");
    }

    #[test]
    fn campaign_serializes_seeds_coverage_findings_artifacts_thresholds_and_provenance() {
        let campaign = FuzzCampaign {
            schema: FUZZ_CAMPAIGN_SCHEMA.to_string(),
            version: FUZZ_CONTRACT_VERSION,
            id: "campaign-1".to_string(),
            title: Some("generic campaign".to_string()),
            safety_class: FuzzSafetyClass::ReadOnly,
            surfaces: Vec::new(),
            targets: vec![FuzzTarget {
                schema: FUZZ_TARGET_SCHEMA.to_string(),
                id: "target-1".to_string(),
                kind: "api".to_string(),
                label: None,
                locator: Some("https://example.test/orders".to_string()),
                operations: vec![FuzzOperation {
                    id: "operation-1".to_string(),
                    kind: "read".to_string(),
                    target_id: Some("target-1".to_string()),
                    label: None,
                    tags: Vec::new(),
                }],
                metadata: Value::Null,
                extra: BTreeMap::new(),
            }],
            workloads: vec![FuzzWorkload {
                schema: FUZZ_WORKLOAD_SCHEMA.to_string(),
                id: "workload-1".to_string(),
                label: None,
                safety_class: FuzzSafetyClass::ReadOnly,
                surface_ids: vec!["surface-1".to_string()],
                operations: vec!["read".to_string()],
                seed_ids: vec!["seed-1".to_string()],
                case_budget: Some(100),
                duration_budget_seconds: Some(60),
                thresholds: Vec::new(),
                metadata: Value::Null,
                extra: BTreeMap::new(),
            }],
            cases: vec![FuzzCase {
                schema: FUZZ_CASE_SCHEMA.to_string(),
                id: "case-1".to_string(),
                target_id: Some("target-1".to_string()),
                operation_id: Some("operation-1".to_string()),
                workload_id: Some("workload-1".to_string()),
                seed_id: Some("seed-1".to_string()),
                replay_id: Some("replay-1".to_string()),
                input: serde_json::json!({ "path": "/orders" }),
                expected: Value::Null,
                observed: Value::Null,
                metadata: Value::Null,
                extra: BTreeMap::new(),
            }],
            seeds: vec![FuzzSeed {
                schema: FUZZ_SEED_SCHEMA.to_string(),
                id: "seed-1".to_string(),
                kind: "literal".to_string(),
                label: None,
                value: Some("sample".to_string()),
                artifact: None,
                tags: Vec::new(),
                metadata: Value::Null,
                extra: BTreeMap::new(),
            }],
            coverage: vec![FuzzCoverage {
                schema: FUZZ_COVERAGE_SCHEMA.to_string(),
                id: "coverage-1".to_string(),
                metric: "operation".to_string(),
                covered: 8,
                total: 10,
                ratio: Some(0.8),
                gaps: vec![FuzzCoverageGap {
                    id: "gap-1".to_string(),
                    label: None,
                    surface_id: Some("surface-1".to_string()),
                    target_id: Some("target-1".to_string()),
                    operation: Some("write".to_string()),
                    operation_id: Some("operation-1".to_string()),
                }],
                metadata: Value::Null,
                extra: BTreeMap::new(),
            }],
            findings: vec![FuzzFinding {
                schema: FUZZ_FINDING_SCHEMA.to_string(),
                id: "finding-1".to_string(),
                title: "unexpected response".to_string(),
                severity: "high".to_string(),
                status: FuzzFindingStatus::Open,
                surface_id: Some("surface-1".to_string()),
                target_id: Some("target-1".to_string()),
                operation_id: Some("operation-1".to_string()),
                case_id: Some("case-1".to_string()),
                workload_id: Some("workload-1".to_string()),
                seed_id: Some("seed-1".to_string()),
                fingerprint: Some("abc123".to_string()),
                artifact_ids: vec!["artifact-1".to_string()],
                metadata: Value::Null,
                extra: BTreeMap::new(),
            }],
            artifacts: vec![FuzzArtifact {
                schema: FUZZ_ARTIFACT_SCHEMA.to_string(),
                id: "artifact-1".to_string(),
                kind: "log".to_string(),
                artifact: None,
                metadata: Value::Null,
                extra: BTreeMap::new(),
            }],
            thresholds: vec![FuzzThreshold {
                schema: FUZZ_THRESHOLD_SCHEMA.to_string(),
                id: "threshold-1".to_string(),
                metric: "coverage_ratio".to_string(),
                operator: FuzzThresholdOperator::GreaterThanOrEqual,
                value: 0.75,
                unit: None,
                safety_class: Some(FuzzSafetyClass::ReadOnly),
                metadata: Value::Null,
                extra: BTreeMap::new(),
            }],
            provenance: Some(FuzzProvenance {
                schema: FUZZ_PROVENANCE_SCHEMA.to_string(),
                producer: "runner".to_string(),
                producer_version: Some("1.0.0".to_string()),
                invocation: Some("run campaign-1".to_string()),
                run_id: Some("run-1".to_string()),
                source_ref: Some("abc123".to_string()),
                created_at: Some("2026-06-21T00:00:00Z".to_string()),
                metadata: Value::Null,
                extra: BTreeMap::new(),
            }),
            replay: Some(FuzzReplayMetadata {
                schema: FUZZ_REPLAY_SCHEMA.to_string(),
                id: "replay-1".to_string(),
                command: Some("homeboy fuzz run component --workload workload-1".to_string()),
                args: Vec::new(),
                env: vec!["HOMEBOY_FUZZ_SEED=sample".to_string()],
                seed: Some("sample".to_string()),
                artifact_id: Some("artifact-1".to_string()),
                metadata: Value::Null,
                extra: BTreeMap::new(),
            }),
            metadata: Value::Null,
            extra: BTreeMap::new(),
        };

        let value = serde_json::to_value(campaign).expect("campaign json");

        assert_eq!(value["schema"], FUZZ_CAMPAIGN_SCHEMA);
        assert_eq!(value["version"], FUZZ_CONTRACT_VERSION);
        assert_eq!(value["targets"][0]["schema"], FUZZ_TARGET_SCHEMA);
        assert_eq!(value["workloads"][0]["schema"], FUZZ_WORKLOAD_SCHEMA);
        assert_eq!(value["cases"][0]["schema"], FUZZ_CASE_SCHEMA);
        assert_eq!(value["seeds"][0]["schema"], FUZZ_SEED_SCHEMA);
        assert_eq!(value["coverage"][0]["schema"], FUZZ_COVERAGE_SCHEMA);
        assert_eq!(value["findings"][0]["schema"], FUZZ_FINDING_SCHEMA);
        assert_eq!(value["artifacts"][0]["schema"], FUZZ_ARTIFACT_SCHEMA);
        assert_eq!(value["thresholds"][0]["schema"], FUZZ_THRESHOLD_SCHEMA);
        assert_eq!(value["provenance"]["schema"], FUZZ_PROVENANCE_SCHEMA);
        assert_eq!(value["replay"]["schema"], FUZZ_REPLAY_SCHEMA);
    }

    #[test]
    fn parse_fuzz_results_file_reads_campaign_contract() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("fuzz-results.json");
        std::fs::write(
            &path,
            serde_json::json!({
                "schema": FUZZ_CAMPAIGN_SCHEMA,
                "id": "campaign-1",
                "safety_class": "read_only"
            })
            .to_string(),
        )
        .expect("write fuzz results");

        let parsed = parse_fuzz_results_file(&path).expect("parse fuzz results");

        assert_eq!(parsed.id, "campaign-1");
        assert_eq!(parsed.safety_class, FuzzSafetyClass::ReadOnly);
    }
}
