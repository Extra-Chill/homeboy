use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::normalize::{
    normalize_optional_string, normalize_string_vec, require_schema, required_trimmed,
    trim_or_default,
};
use super::schema_defaults::{fuzz_contract_version, fuzz_hotspot_set_schema};
use super::schemas::{FUZZ_CONTRACT_VERSION, FUZZ_HOTSPOT_SET_SCHEMA};
use super::FuzzProvenance;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FuzzHotspotSet {
    #[serde(default = "fuzz_hotspot_set_schema")]
    pub schema: String,
    #[serde(default = "fuzz_contract_version")]
    pub version: u32,
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub items: Vec<FuzzHotspot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<FuzzProvenance>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

impl FuzzHotspotSet {
    pub fn from_value(value: Value) -> std::result::Result<Self, String> {
        let mut set: Self = serde_json::from_value(value).map_err(|err| err.to_string())?;
        set.normalize()?;
        Ok(set)
    }

    pub fn normalize(&mut self) -> std::result::Result<(), String> {
        self.schema = trim_or_default(&self.schema, FUZZ_HOTSPOT_SET_SCHEMA);
        require_schema(&self.schema, FUZZ_HOTSPOT_SET_SCHEMA, "fuzz hotspot set")?;
        if self.version != FUZZ_CONTRACT_VERSION {
            return Err(format!(
                "fuzz hotspot set version must be {FUZZ_CONTRACT_VERSION}"
            ));
        }
        self.id = required_trimmed("hotspot_set.id", &self.id)?;
        self.label = normalize_optional_string(self.label.take());
        for item in &mut self.items {
            item.normalize()?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FuzzHotspot {
    pub id: String,
    pub dimension: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    pub metric: String,
    pub value: f64,
    pub unit: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub basis: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample_count: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rank: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relative_score: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_refs: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_refs: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_refs: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<FuzzProvenance>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

impl FuzzHotspot {
    fn normalize(&mut self) -> std::result::Result<(), String> {
        self.id = required_trimmed("hotspot.id", &self.id)?;
        self.dimension = required_trimmed("hotspot.dimension", &self.dimension)?;
        self.kind = normalize_optional_string(self.kind.take());
        self.metric = required_trimmed("hotspot.metric", &self.metric)?;
        if !self.value.is_finite() {
            return Err(format!("hotspot.value must be finite for `{}`", self.id));
        }
        self.unit = required_trimmed("hotspot.unit", &self.unit)?;
        self.basis = normalize_optional_string(self.basis.take());
        if let Some(relative_score) = self.relative_score {
            if !relative_score.is_finite() {
                return Err(format!(
                    "hotspot.relative_score must be finite for `{}`",
                    self.id
                ));
            }
        }
        self.label = normalize_optional_string(self.label.take());
        self.labels = normalize_string_vec(std::mem::take(&mut self.labels));
        self.evidence_refs = normalize_string_vec(std::mem::take(&mut self.evidence_refs));
        self.artifact_refs = normalize_string_vec(std::mem::take(&mut self.artifact_refs));
        self.source_refs = normalize_string_vec(std::mem::take(&mut self.source_refs));
        Ok(())
    }
}

pub fn parse_fuzz_hotspot_set_value(value: &Value) -> Option<FuzzHotspotSet> {
    let candidate = if value.get("schema").and_then(Value::as_str) == Some(FUZZ_HOTSPOT_SET_SCHEMA)
    {
        Some(value.clone())
    } else {
        value
            .get("hotspots")
            .filter(|hotspots| {
                hotspots.get("schema").and_then(Value::as_str) == Some(FUZZ_HOTSPOT_SET_SCHEMA)
            })
            .cloned()
    }?;
    FuzzHotspotSet::from_value(candidate).ok()
}
