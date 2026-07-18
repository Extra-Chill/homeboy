//! Trace-workload schema for rig specs — variant/profile/guardrail/experiment
//! and public-preview tunnel declarations consumed by `homeboy trace`.

pub use homeboy_rig_contract::{
    TraceDependencySpec, TraceNativePublicPreviewSpec, TracePreviewAssetFanoutSpec,
    TracePublicPreviewMode, TracePublicPreviewSpec,
};

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use super::CheckSpec;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraceVariantSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub component: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overlay: Option<String>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub overlays: Vec<TraceVariantOverlaySpec>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub trace_guardrails: Vec<TraceGuardrailSpec>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TraceProfileSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub component: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scenario: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rig: Option<String>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub overlays: Vec<String>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub variants: Vec<String>,

    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub settings: BTreeMap<String, serde_json::Value>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_env: Vec<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compare_bundle: Option<TraceCompareBundleProfileSpec>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_preview: Option<TracePublicPreviewSpec>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TraceCompareBundleProfileSpec {
    pub component: String,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scenarios: Vec<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repeat: Option<usize>,

    #[serde(default)]
    pub canonical: bool,
}

impl TraceProfileSpec {
    pub fn string_settings(&self) -> Vec<(String, String)> {
        self.settings
            .iter()
            .filter_map(|(key, value)| {
                value
                    .as_str()
                    .map(|string| (key.clone(), string.to_string()))
            })
            .collect()
    }

    pub fn json_settings(&self) -> Vec<(String, serde_json::Value)> {
        self.settings
            .iter()
            .filter(|(_, value)| !value.is_string())
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraceGuardrailSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,

    #[serde(flatten)]
    pub check: CheckSpec,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraceVariantOverlaySpec {
    pub component: String,
    pub overlay: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TraceExperimentSpec {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub setup: Vec<TraceExperimentCommandSpec>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub teardown: Vec<TraceExperimentCommandSpec>,

    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub settings: BTreeMap<String, serde_json::Value>,

    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<TraceExperimentArtifactSpec>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraceExperimentCommandSpec {
    pub command: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,

    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum TraceExperimentArtifactSpec {
    Path(String),
    Detailed { label: String, path: String },
}
