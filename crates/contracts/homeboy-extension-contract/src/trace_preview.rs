//! Pure trace preview metadata contract types.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TracePreviewAssetCheck {
    pub path: String,
    pub url: String,
    pub status: Option<u16>,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TracePreviewAssetFanoutRequest {
    pub path: String,
    pub url: String,
    pub status: Option<u16>,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_bucket: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TracePreviewAssetFanoutReport {
    pub schema: String,
    pub concurrency: usize,
    pub repeat_count: usize,
    pub asset_path_count: usize,
    pub expected_request_count: usize,
    pub client_request_count: usize,
    pub success_count: usize,
    pub failure_count: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ingress_request_count: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_origin_request_count: Option<usize>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub status_counts: BTreeMap<String, usize>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub failure_buckets: BTreeMap<String, usize>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requests: Vec<TracePreviewAssetFanoutRequest>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TracePreviewMetadata {
    pub schema: String,
    pub requested_mode: String,
    pub provider: String,
    pub local_origin: String,
    pub local_url: String,
    pub public_origin: String,
    pub public_url: String,
    pub browser_effective_origin: String,
    pub window_location_origin: String,
    pub window_is_secure_context: bool,
    pub require_https: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_assets: Vec<TracePreviewAssetCheck>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asset_fanout: Option<TracePreviewAssetFanoutReport>,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_id: Option<String>,
    pub cleanup_status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_host: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ingress_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_paths: Option<TracePreviewLogPaths>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TracePreviewLogPaths {
    pub client_stdout_path: String,
    pub client_stderr_path: String,
}
