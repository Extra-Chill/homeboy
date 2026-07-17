use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::normalize::{require_schema, required_trimmed, trim_or_default};
use super::schema_defaults::{fuzz_contract_version, fuzz_observation_set_schema};
use super::schemas::{FUZZ_CONTRACT_VERSION, FUZZ_OBSERVATION_SET_SCHEMA};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FuzzObservationSet {
    #[serde(default = "fuzz_observation_set_schema")]
    pub schema: String,
    #[serde(default = "fuzz_contract_version")]
    pub version: u32,
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub observations: Vec<FuzzObservation>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

impl FuzzObservationSet {
    pub fn from_value(value: Value) -> std::result::Result<Self, String> {
        let mut set: Self = serde_json::from_value(value).map_err(|err| err.to_string())?;
        set.normalize()?;
        Ok(set)
    }

    fn normalize(&mut self) -> std::result::Result<(), String> {
        self.schema = trim_or_default(&self.schema, FUZZ_OBSERVATION_SET_SCHEMA);
        require_schema(
            &self.schema,
            FUZZ_OBSERVATION_SET_SCHEMA,
            "fuzz observation set",
        )?;
        if self.version != FUZZ_CONTRACT_VERSION {
            return Err(format!(
                "fuzz observation set version must be {FUZZ_CONTRACT_VERSION}"
            ));
        }
        self.id = required_trimmed("observation_set.id", &self.id)?;
        for observation in &mut self.observations {
            observation.normalize()?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FuzzObservation {
    pub id: String,
    pub family: FuzzObservationFamily,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub case_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    pub subject: String,
    pub metric: String,
    pub value: f64,
    pub unit: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample_count: Option<u64>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

impl FuzzObservation {
    fn normalize(&mut self) -> std::result::Result<(), String> {
        self.id = required_trimmed("observation.id", &self.id)?;
        self.case_id = normalize_optional_string(self.case_id.take());
        self.target_id = normalize_optional_string(self.target_id.take());
        self.operation_id = normalize_optional_string(self.operation_id.take());
        self.phase = normalize_optional_string(self.phase.take());
        self.subject = required_trimmed("observation.subject", &self.subject)?;
        self.metric = required_trimmed("observation.metric", &self.metric)?;
        if !self.value.is_finite() {
            return Err(format!(
                "observation.value must be finite for `{}`",
                self.id
            ));
        }
        self.unit = required_trimmed("observation.unit", &self.unit)?;
        self.fingerprint = normalize_optional_string(self.fingerprint.take());
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FuzzObservationFamily {
    Action,
    Query,
    Resource,
    Timing,
    Counter,
}

fn normalize_optional_string(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    })
}

pub fn parse_fuzz_observation_set_value(value: &Value) -> Option<FuzzObservationSet> {
    let candidate = if value.get("schema").and_then(Value::as_str)
        == Some(FUZZ_OBSERVATION_SET_SCHEMA)
    {
        Some(value)
    } else {
        value
            .get("observation_set")
            .or_else(|| value.get("observations"))
            .filter(|candidate| {
                candidate.get("schema").and_then(Value::as_str) == Some(FUZZ_OBSERVATION_SET_SCHEMA)
            })
    }?;
    FuzzObservationSet::from_value(candidate.clone()).ok()
}
