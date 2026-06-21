use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use super::headers::deserialize_response_headers;

#[derive(Debug, Clone)]
pub struct PreviewClientStartSpec {
    pub ingress: String,
    pub public_host: String,
    pub local_origin: String,
    pub session_id: Option<String>,
    pub token_env: String,
    pub poll_timeout_secs: u64,
    pub ready_stdout: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PreviewClientReport {
    pub command: &'static str,
    pub ingress: String,
    pub public_host: String,
    pub local_origin: String,
    pub registered: bool,
    pub stopped: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PreviewClientAuthDiagnostic {
    pub command: &'static str,
    pub token_env: String,
    pub token_present: bool,
    pub token_empty: bool,
    pub local_token_sha256: Option<String>,
    pub expected_sha256_env: String,
    pub expected_sha256: Option<String>,
    pub matches_expected: Option<bool>,
    pub hashing_semantics: &'static str,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PreviewIngressRequest {
    pub request_id: String,
    pub method: String,
    pub path: String,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_base64: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PreviewIngressNextResponse {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request: Option<PreviewIngressRequest>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PreviewIngressResponse {
    pub request_id: String,
    pub status: u16,
    #[serde(default, deserialize_with = "deserialize_response_headers")]
    pub headers: Vec<(String, String)>,
    #[serde(default)]
    pub body_base64: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub body_stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<PreviewClientForwardError>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PreviewIngressResponseChunk {
    pub request_id: String,
    pub sequence: u64,
    pub body_base64: String,
    #[serde(default)]
    pub complete: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PreviewClientForwardError {
    pub kind: String,
    pub message: String,
}
