//! Fuzz workload manifest config types.

use crate::runtime_helper::RuntimeHelperRequirement;
use homeboy_lifecycle_contract::LifecycleContract;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct FuzzConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extension_script: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env: Vec<String>,
    /// Core-owned helper capabilities required by this fuzz runner.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runtime_helpers: Vec<RuntimeHelperRequirement>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workloads: Vec<FuzzWorkloadConfig>,
    /// Extension-owned JSON values that declare a workload when present.
    ///
    /// This keeps ecosystem-specific manifest paths and JSON pointers in the
    /// extension contract while core performs only generic JSON lookup.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workload_json_probes: Vec<FuzzWorkloadJsonProbe>,
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FuzzWorkloadJsonProbe {
    /// JSON file relative to the component root.
    pub path: String,
    /// RFC 6901 JSON Pointer whose non-empty string value declares the workload.
    pub pointer: String,
    /// Stable workload identifier emitted when the pointer resolves.
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}
