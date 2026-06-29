//! Target inventory, execution request, and result envelope contracts.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::coverage::{FuzzArtifact, FuzzProvenance, FuzzThresholdOperator};
use super::normalize::{require_schema, required_trimmed, trim_or_default};
use super::schema_defaults::{
    fuzz_contract_version, fuzz_execution_request_schema, fuzz_gate_schema,
    fuzz_required_artifact_schema, fuzz_result_envelope_schema, fuzz_sampling_request_schema,
    fuzz_target_inventory_schema, isolation_proof_schema,
};
use super::schemas::{FUZZ_CONTRACT_VERSION, FUZZ_TARGET_INVENTORY_SCHEMA, ISOLATION_PROOF_SCHEMA};
use super::types::{
    FuzzCampaign, FuzzSeed, FuzzSequencePlan, FuzzSurface, FuzzTarget, FuzzWorkload,
};

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
    #[serde(default)]
    pub sampling: FuzzSamplingRequest,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sequence_plan: Option<FuzzSequencePlan>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub isolation_proof: Option<IsolationProof>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IsolationProof {
    #[serde(default = "isolation_proof_schema")]
    pub schema: String,
    #[serde(default = "fuzz_contract_version")]
    pub version: u32,
    pub runtime_kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_ref: Option<Value>,
    pub disposable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reset_supported: Option<bool>,
    pub teardown_required: bool,
    pub mutation_boundary: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub proof_artifacts: Vec<Value>,
    pub verified_by: String,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

impl IsolationProof {
    pub fn from_value(value: Value) -> std::result::Result<Self, String> {
        let mut proof: Self = serde_json::from_value(value).map_err(|err| err.to_string())?;
        proof.normalize()?;
        Ok(proof)
    }

    pub fn normalize(&mut self) -> std::result::Result<(), String> {
        self.schema = trim_or_default(&self.schema, ISOLATION_PROOF_SCHEMA);
        require_schema(&self.schema, ISOLATION_PROOF_SCHEMA, "isolation proof")?;
        if self.version != FUZZ_CONTRACT_VERSION {
            return Err(format!(
                "isolation proof version must be {FUZZ_CONTRACT_VERSION}"
            ));
        }
        self.runtime_kind = required_trimmed("isolation_proof.runtime_kind", &self.runtime_kind)?;
        self.snapshot_ref = self
            .snapshot_ref
            .as_deref()
            .map(|snapshot_ref| required_trimmed("isolation_proof.snapshot_ref", snapshot_ref))
            .transpose()?;
        self.mutation_boundary =
            required_trimmed("isolation_proof.mutation_boundary", &self.mutation_boundary)?;
        self.verified_by = required_trimmed("isolation_proof.verified_by", &self.verified_by)?;
        if !self.disposable {
            return Err("isolation_proof.disposable must be true".to_string());
        }
        if !self.teardown_required {
            return Err("isolation_proof.teardown_required must be true".to_string());
        }
        if self.proof_artifacts.is_empty() {
            return Err("isolation_proof.proof_artifacts must be non-empty".to_string());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FuzzSamplingRequest {
    #[serde(default = "fuzz_sampling_request_schema")]
    pub schema: String,
    #[serde(default = "default_sampling_strategy")]
    pub strategy: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub case_budget: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_budget_seconds: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub target_strata: Vec<FuzzSamplingStratum>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub operation_strata: Vec<FuzzSamplingStratum>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub corpus_refs: Vec<FuzzSamplingCorpusRef>,
    #[serde(default)]
    pub replay: FuzzSamplingReplayDeterminism,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

impl Default for FuzzSamplingRequest {
    fn default() -> Self {
        Self {
            schema: fuzz_sampling_request_schema(),
            strategy: default_sampling_strategy(),
            seed: None,
            case_budget: None,
            duration_budget_seconds: None,
            target_strata: Vec::new(),
            operation_strata: Vec::new(),
            corpus_refs: Vec::new(),
            replay: FuzzSamplingReplayDeterminism::default(),
            metadata: Value::Null,
            extra: BTreeMap::new(),
        }
    }
}

fn default_sampling_strategy() -> String {
    "all".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FuzzSamplingStratum {
    pub id: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub values: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FuzzSamplingCorpusRef {
    pub id: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact: Option<Value>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub has_inline_value: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FuzzSamplingReplayDeterminism {
    #[serde(default = "default_replay_deterministic")]
    pub deterministic: bool,
    #[serde(default = "default_replay_seed_source")]
    pub seed_source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replay_batch_id: Option<String>,
}

impl Default for FuzzSamplingReplayDeterminism {
    fn default() -> Self {
        Self {
            deterministic: default_replay_deterministic(),
            seed_source: default_replay_seed_source(),
            replay_batch_id: None,
        }
    }
}

fn default_replay_deterministic() -> bool {
    true
}

fn default_replay_seed_source() -> String {
    "unspecified".to_string()
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
