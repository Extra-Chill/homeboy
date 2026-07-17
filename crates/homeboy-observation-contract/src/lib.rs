//! Shared observation record types for homeboy.
//!
//! Behavior-free data describing recorded run artifacts. These live below core so
//! consumers — the observation store that persists them and report layers like
//! the extension bench commands that carry them — can share the vocabulary
//! without depending on the observation store.

use serde::{Deserialize, Serialize};

/// A recorded artifact produced by a run: its identity, on-disk path, optional
/// URLs/viewer links, and metadata.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArtifactRecord {
    pub id: String,
    pub run_id: String,
    pub kind: String,
    #[serde(rename = "type", default = "default_artifact_type")]
    pub artifact_type: String,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub viewer_url: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub viewer_links: Vec<ArtifactViewerLink>,
    pub sha256: Option<String>,
    pub size_bytes: Option<i64>,
    pub mime: Option<String>,
    #[serde(default)]
    pub metadata_json: serde_json::Value,
    pub created_at: String,
}

/// A viewer link for a recorded artifact (e.g. a trace viewer or replay URL).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArtifactViewerLink {
    pub kind: String,
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replay: Option<serde_json::Value>,
}

fn default_artifact_type() -> String {
    "file".to_string()
}
