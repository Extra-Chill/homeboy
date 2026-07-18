//! Fuzz workload manifest config types.

use homeboy_lifecycle_contract::LifecycleContract;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct FuzzConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extension_script: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workloads: Vec<FuzzWorkloadConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub case_artifact: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub corpus_artifacts: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replay_command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minimize_command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_schema: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_retention: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FuzzWorkloadConfig {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifecycle: Option<LifecycleContract>,
}
