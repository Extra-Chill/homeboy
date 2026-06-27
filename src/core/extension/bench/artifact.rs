//! Bench scenario artifact pointers.

use serde::{Deserialize, Serialize};

use crate::core::observation::ArtifactViewerLink;

/// Viewer pointers shared by bench artifact records and the compact
/// artifact index. Embedded via `#[serde(flatten)]` so the on-wire JSON
/// keeps `viewer_url` / `viewer_links` at the parent level — identical
/// shape to the previous inline fields.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct BenchArtifactViewer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub viewer_url: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub viewer_links: Vec<ArtifactViewerLink>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct BenchPreviewLifecycleMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cleanup_status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_lifecycle: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub browser_origin_evidence: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct BenchArtifact {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub artifact_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observation_artifact_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub viewer: Option<serde_json::Value>,
    #[serde(flatten)]
    pub viewer_refs: BenchArtifactViewer,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(flatten)]
    pub preview_lifecycle: BenchPreviewLifecycleMetadata,
}

#[cfg(test)]
#[path = "../../../../tests/core/extension/bench/artifact_test.rs"]
mod artifact_test;
