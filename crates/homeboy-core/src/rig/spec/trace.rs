//! Trace-workload schema for rig specs — variant/profile/guardrail/experiment
//! and public-preview tunnel declarations consumed by `homeboy trace`.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use super::CheckSpec;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TraceDependencySpec {
    pub id: String,
    pub kind: String,
    pub source: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin_file: Option<String>,

    #[serde(default)]
    pub requires_built_assets: bool,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_paths: Vec<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_url: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r#ref: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package_marker: Option<String>,
}

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TracePublicPreviewSpec {
    /// Preview provider lifecycle mode. Defaults to the existing external
    /// command/public-origin behavior.
    #[serde(default)]
    pub mode: TracePublicPreviewMode,

    /// Local HTTP origin to expose, for example `http://127.0.0.1:8888`.
    pub local_origin: String,

    /// Optional already-known public origin. When omitted, Homeboy starts
    /// `command` and reads the first HTTPS URL printed to stdout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_origin: Option<String>,

    /// Long-running shell command that starts the tunnel provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,

    /// Fail before the trace runner starts unless the effective origin is HTTPS.
    #[serde(default)]
    pub require_https: bool,

    /// Human-readable provider label for artifacts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,

    /// Seconds to wait for the provider command to print a public HTTPS URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub startup_timeout_seconds: Option<u64>,

    /// Public-origin-relative asset URLs that must load before trace collection starts.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_asset_paths: Vec<String>,

    /// Optional concurrent static-asset fanout check for public preview tunnels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asset_fanout: Option<TracePreviewAssetFanoutSpec>,

    /// Homeboy-native preview tunnel settings used when `mode` is `homeboy_native`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native: Option<TraceNativePublicPreviewSpec>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TracePreviewAssetFanoutSpec {
    /// Public-origin-relative asset URLs to fetch concurrently through the preview origin.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub asset_paths: Vec<String>,

    /// Maximum number of concurrent fetch workers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub concurrency: Option<usize>,

    /// Number of times to request each asset path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repeat_count: Option<usize>,

    /// Optional body substring that every successful response must contain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_body_contains: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum TracePublicPreviewMode {
    #[default]
    External,
    HomeboyNative,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct TraceNativePublicPreviewSpec {
    /// Deterministic public host reserved by the native preview ingress.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_host: Option<String>,

    /// Operator-owned wildcard domain used to derive `{session}-tunnel.<domain>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operator_domain: Option<String>,

    /// Stable session ID used for host generation and native client metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,

    /// Native preview ingress/broker URL consumed by `homeboy tunnel preview-client start`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ingress_url: Option<String>,

    /// Environment variable name that supplies the native preview tunnel token.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_env: Option<String>,

    /// Optional path to a Homeboy binary that implements the preview-client command.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_binary: Option<String>,
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
