//! Target inventory, execution request, and result envelope contracts.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::coverage::{FuzzArtifact, FuzzProvenance, FuzzThresholdOperator};
use super::normalize::{require_schema, required_trimmed, trim_or_default};
use super::schema_defaults::{
    fuzz_contract_version, fuzz_execution_request_schema, fuzz_gate_schema,
    fuzz_required_artifact_schema, fuzz_result_envelope_schema, fuzz_target_inventory_schema,
};
use super::schemas::{FUZZ_CONTRACT_VERSION, FUZZ_TARGET_INVENTORY_SCHEMA};
use super::types::{FuzzCampaign, FuzzSeed, FuzzSurface, FuzzTarget, FuzzWorkload};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FuzzTargetInventory {
    #[serde(default = "fuzz_target_inventory_schema")]
    pub schema: String,
    #[serde(default = "fuzz_contract_version")]
    pub version: u32,
    pub id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub surfaces: Vec<FuzzSurface>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub targets: Vec<FuzzTarget>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workloads: Vec<FuzzWorkload>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub seeds: Vec<FuzzSeed>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<FuzzProvenance>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

impl FuzzTargetInventory {
    pub fn from_value(value: Value) -> std::result::Result<Self, String> {
        let mut inventory: Self = serde_json::from_value(value).map_err(|err| err.to_string())?;
        inventory.normalize()?;
        Ok(inventory)
    }

    pub(super) fn normalize(&mut self) -> std::result::Result<(), String> {
        self.schema = trim_or_default(&self.schema, FUZZ_TARGET_INVENTORY_SCHEMA);
        require_schema(
            &self.schema,
            FUZZ_TARGET_INVENTORY_SCHEMA,
            "fuzz target inventory",
        )?;
        if self.version != FUZZ_CONTRACT_VERSION {
            return Err(format!(
                "fuzz target inventory version must be {FUZZ_CONTRACT_VERSION}"
            ));
        }
        self.id = required_trimmed("inventory.id", &self.id)?;
        for surface in &mut self.surfaces {
            surface.normalize()?;
        }
        for target in &mut self.targets {
            target.normalize()?;
        }
        for workload in &mut self.workloads {
            workload.normalize()?;
        }
        for seed in &mut self.seeds {
            seed.normalize()?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FuzzExecutionRequest {
    #[serde(default = "fuzz_execution_request_schema")]
    pub schema: String,
    #[serde(default = "fuzz_contract_version")]
    pub version: u32,
    pub id: String,
    pub component: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rig_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workload_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub case_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_duration: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_artifacts: Vec<FuzzRequiredArtifact>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gates: Vec<FuzzGate>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FuzzResultEnvelope {
    #[serde(default = "fuzz_result_envelope_schema")]
    pub schema: String,
    #[serde(default = "fuzz_contract_version")]
    pub version: u32,
    pub id: String,
    pub status: String,
    pub request: FuzzExecutionRequest,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub campaign: Option<FuzzCampaign>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<FuzzArtifact>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_artifacts: Vec<FuzzRequiredArtifact>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gates: Vec<FuzzGate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<FuzzProvenance>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FuzzRequiredArtifact {
    #[serde(default = "fuzz_required_artifact_schema")]
    pub schema: String,
    pub id: String,
    pub kind: String,
    pub required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub acceptable_artifact_kinds: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FuzzGate {
    #[serde(default = "fuzz_gate_schema")]
    pub schema: String,
    pub id: String,
    pub kind: String,
    pub metric: String,
    pub operator: FuzzThresholdOperator,
    pub value: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}
