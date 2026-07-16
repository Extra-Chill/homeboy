use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extension_script: Option<String>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runner_capabilities: Vec<String>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub toolchain_provenance: Vec<TraceToolchainProvenanceConfig>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub browser_evidence: Vec<TraceBrowserEvidenceAdapterConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TraceBrowserEvidenceAdapterConfig {
    pub id: String,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub summary_aliases: Vec<TraceBrowserSummaryAliasConfig>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_maps: Vec<TraceBrowserArtifactMapConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TraceBrowserSummaryAliasConfig {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub request_total_keys: Vec<String>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub page_error_keys: Vec<String>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub metrics: Vec<TraceBrowserMetricAliasConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TraceBrowserMetricAliasConfig {
    pub metric: String,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keys: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TraceBrowserArtifactMapConfig {
    pub field: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TraceToolchainProvenanceConfig {
    pub id: String,
    pub label: String,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env_keys: Vec<String>,
}
