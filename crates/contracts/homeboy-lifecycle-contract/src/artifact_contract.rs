//! Product-neutral artifact and evidence contract primitives.
//!
//! These contracts intentionally stay small and generic. Producers may carry
//! domain-specific fields through `extra`, while core owns the shared schema,
//! field aliases, and non-empty target normalization.

use std::collections::BTreeMap;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArtifactViewerLink {
    pub kind: String,
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replay: Option<serde_json::Value>,
}

/// A recorded artifact produced by a run: its identity, on-disk path, optional
/// URLs/viewer links, and metadata. A behavior-free record type shared by the
/// observation store that persists it and report layers that carry it.
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

fn default_artifact_type() -> String {
    "file".to_string()
}

impl Default for ArtifactRecord {
    /// A defaulted record has empty `id`/`run_id`/`kind`/`path`/`created_at`,
    /// `artifact_type` = `"file"` (matching the serde default so a defaulted and
    /// a deserialized-with-omitted-type record agree), and every optional field
    /// `None`/empty/`Value::Null`. Construct with `..Default::default()` and set
    /// the meaningful fields to drop the boilerplate every call site otherwise
    /// repeats.
    fn default() -> Self {
        Self {
            id: String::new(),
            run_id: String::new(),
            kind: String::new(),
            artifact_type: default_artifact_type(),
            path: String::new(),
            url: None,
            public_url: None,
            viewer_url: None,
            viewer_links: Vec::new(),
            sha256: None,
            size_bytes: None,
            mime: None,
            metadata_json: serde_json::Value::Null,
            created_at: String::new(),
        }
    }
}

pub const ARTIFACT_CONTRACT_SCHEMA: &str = "homeboy/artifact-contract/v1";
pub const EVIDENCE_CONTRACT_SCHEMA: &str = "homeboy/evidence-contract/v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArtifactContract {
    #[serde(default = "artifact_contract_schema")]
    pub schema: String,
    pub kind: String,
    #[serde(
        rename = "type",
        alias = "artifact_type",
        default = "default_artifact_type"
    )]
    pub artifact_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

impl ArtifactContract {
    pub fn from_value(value: Value) -> Result<Self, String> {
        contract_from_value(value, Self::normalize)
    }

    pub fn target(&self) -> Option<&str> {
        self.public_url
            .as_deref()
            .or(self.url.as_deref())
            .or(self.path.as_deref())
    }

    fn normalize(&mut self) -> Result<(), String> {
        normalize_schema_and_kind(
            &mut self.schema,
            &mut self.kind,
            ARTIFACT_CONTRACT_SCHEMA,
            "artifact contract",
        )?;
        self.artifact_type = trim_or_default(&self.artifact_type, "file");
        self.path = normalize_optional_string(self.path.take());
        self.url = normalize_optional_string(self.url.take());
        self.public_url = normalize_optional_string(self.public_url.take());
        self.role = normalize_optional_string(self.role.take());
        self.label = normalize_optional_string(self.label.take());
        self.semantic_key = normalize_optional_string(self.semantic_key.take());
        if self.metadata.is_null() {
            self.metadata = Value::Null;
        }
        if self.target().is_none() {
            return Err(
                "artifact contract must include a non-empty path, url, or public_url".to_string(),
            );
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EvidenceContract {
    #[serde(default = "evidence_contract_schema")]
    pub schema: String,
    pub kind: String,
    pub target: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact: Option<ArtifactContract>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

impl EvidenceContract {
    pub fn from_value(value: Value) -> Result<Self, String> {
        contract_from_value(value, Self::normalize)
    }

    fn normalize(&mut self) -> Result<(), String> {
        normalize_schema_and_kind(
            &mut self.schema,
            &mut self.kind,
            EVIDENCE_CONTRACT_SCHEMA,
            "evidence contract",
        )?;
        self.target = required_trimmed("target", &self.target)?;
        self.label = trim_or_default(&self.label, &self.kind);
        self.role = normalize_optional_string(self.role.take());
        self.semantic_key = normalize_optional_string(self.semantic_key.take());
        if let Some(artifact) = &mut self.artifact {
            artifact.normalize()?;
        }
        Ok(())
    }
}

fn normalize_schema_and_kind(
    schema: &mut String,
    kind: &mut String,
    expected_schema: &str,
    label: &str,
) -> Result<(), String> {
    *schema = trim_or_default(schema, expected_schema);
    require_schema(schema, expected_schema, label)?;
    *kind = required_trimmed("kind", kind)?;
    Ok(())
}

fn contract_from_value<T, F>(value: Value, normalize: F) -> Result<T, String>
where
    T: DeserializeOwned,
    F: FnOnce(&mut T) -> Result<(), String>,
{
    let mut contract: T = serde_json::from_value(value).map_err(|err| err.to_string())?;
    normalize(&mut contract)?;
    Ok(contract)
}

fn artifact_contract_schema() -> String {
    ARTIFACT_CONTRACT_SCHEMA.to_string()
}

fn evidence_contract_schema() -> String {
    EVIDENCE_CONTRACT_SCHEMA.to_string()
}

fn normalize_optional_string(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    })
}

fn trim_or_default(value: &str, default: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        default.to_string()
    } else {
        trimmed.to_string()
    }
}

fn required_trimmed(field: &str, value: &str) -> Result<String, String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        Err(format!("{field} must be non-empty"))
    } else {
        Ok(trimmed.to_string())
    }
}

fn require_schema(actual: &str, expected: &str, label: &str) -> Result<(), String> {
    if actual == expected {
        Ok(())
    } else {
        Err(format!("{label} schema must be {expected}"))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn artifact_contract_normalizes_aliases_and_empty_fields() {
        let artifact = ArtifactContract::from_value(json!({
            "kind": " transcript ",
            "type": " json ",
            "path": " artifacts/run.json ",
            "url": " ",
            "role": " primary-output ",
            "label": " Run transcript ",
            "semantic_key": " task.transcript ",
            "producer": "extension"
        }))
        .expect("artifact contract");

        assert_eq!(artifact.schema, ARTIFACT_CONTRACT_SCHEMA);
        assert_eq!(artifact.kind, "transcript");
        assert_eq!(artifact.artifact_type, "json");
        assert_eq!(artifact.path.as_deref(), Some("artifacts/run.json"));
        assert_eq!(artifact.url, None);
        assert_eq!(artifact.role.as_deref(), Some("primary-output"));
        assert_eq!(artifact.label.as_deref(), Some("Run transcript"));
        assert_eq!(artifact.semantic_key.as_deref(), Some("task.transcript"));
        assert_eq!(artifact.extra["producer"], "extension");
    }

    #[test]
    fn artifact_contract_requires_a_non_empty_target() {
        let err = ArtifactContract::from_value(json!({
            "kind": "log",
            "path": " "
        }))
        .expect_err("target error");

        assert!(err.contains("path, url, or public_url"));
    }

    #[test]
    fn evidence_contract_normalizes_label_and_nested_artifact() {
        let evidence = EvidenceContract::from_value(json!({
            "kind": " proof ",
            "target": " https://example.test/proof ",
            "label": " ",
            "artifact": {
                "kind": "proof-json",
                "path": "proof.json"
            }
        }))
        .expect("evidence contract");

        assert_eq!(evidence.schema, EVIDENCE_CONTRACT_SCHEMA);
        assert_eq!(evidence.kind, "proof");
        assert_eq!(evidence.label, "proof");
        assert_eq!(evidence.target, "https://example.test/proof");
        assert_eq!(evidence.artifact.expect("artifact").artifact_type, "file");
    }

    #[test]
    fn artifact_record_default_matches_the_fully_spelled_out_empty_record() {
        let via_default = ArtifactRecord {
            id: "frontend_url".to_string(),
            run_id: "run-1".to_string(),
            kind: "frontend_url".to_string(),
            artifact_type: "url".to_string(),
            path: "https://example.test/".to_string(),
            created_at: "2026-06-12T00:00:30Z".to_string(),
            ..Default::default()
        };
        let verbose = ArtifactRecord {
            id: "frontend_url".to_string(),
            run_id: "run-1".to_string(),
            kind: "frontend_url".to_string(),
            artifact_type: "url".to_string(),
            path: "https://example.test/".to_string(),
            url: None,
            public_url: None,
            viewer_url: None,
            viewer_links: Vec::new(),
            sha256: None,
            size_bytes: None,
            mime: None,
            metadata_json: serde_json::Value::Null,
            created_at: "2026-06-12T00:00:30Z".to_string(),
        };
        assert_eq!(via_default, verbose);
    }

    #[test]
    fn artifact_record_default_artifact_type_is_file() {
        assert_eq!(ArtifactRecord::default().artifact_type, "file");
    }
}
