use crate::lifecycle::LifecycleContract;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

mod autofix_config;
mod trace_config;
pub use autofix_config::AutofixVerifyConfig;
pub use trace_config::{
    TraceBrowserArtifactMapConfig, TraceBrowserEvidenceAdapterConfig,
    TraceBrowserMetricAliasConfig, TraceBrowserSummaryAliasConfig, TraceConfig,
    TraceToolchainProvenanceConfig,
};

#[cfg(test)]
mod tests {
    use homeboy_extension_contract::manifest_toolchain_config::DepsConfig;

    #[test]
    fn deps_config_preserves_the_legacy_extension_script_contract() {
        let config: DepsConfig = serde_json::from_str(r#"{"extension_script":"deps.sh"}"#).unwrap();
        assert_eq!(config.extension_script.as_deref(), Some("deps.sh"));
    }
}

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
