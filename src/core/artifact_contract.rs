//! Product-neutral artifact and evidence contract primitives.
//!
//! These contracts intentionally stay small and generic. Producers may carry
//! domain-specific fields through `extra`, while core owns the shared schema,
//! field aliases, and non-empty target normalization.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::core::artifact_ref::ArtifactRef;
use crate::core::observation::ArtifactRecord;

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
        let mut artifact: Self = serde_json::from_value(value).map_err(|err| err.to_string())?;
        artifact.normalize()?;
        Ok(artifact)
    }

    pub fn from_record(record: &ArtifactRecord) -> Self {
        Self {
            schema: ARTIFACT_CONTRACT_SCHEMA.to_string(),
            kind: record.kind.clone(),
            artifact_type: record.artifact_type.clone(),
            path: Some(record.path.clone()),
            url: record.url.clone(),
            public_url: record.public_url.clone(),
            role: None,
            label: None,
            semantic_key: None,
            size_bytes: record
                .size_bytes
                .and_then(|value| u64::try_from(value).ok()),
            sha256: record.sha256.clone(),
            metadata: record.metadata_json.clone(),
            extra: BTreeMap::new(),
        }
    }

    pub fn target(&self) -> Option<&str> {
        self.public_url
            .as_deref()
            .or(self.url.as_deref())
            .or(self.path.as_deref())
    }

    pub fn to_artifact_ref(&self, id: impl Into<String>, run_id: impl Into<String>) -> ArtifactRef {
        ArtifactRef {
            schema: crate::core::artifact_ref::ARTIFACT_REF_SCHEMA.to_string(),
            id: id.into(),
            run_id: run_id.into(),
            kind: self.kind.clone(),
            artifact_type: self.artifact_type.clone(),
            path: self
                .path
                .clone()
                .or_else(|| self.url.clone())
                .or_else(|| self.public_url.clone())
                .unwrap_or_default(),
            url: self.url.clone(),
            public_url: self.public_url.clone(),
            role: self.role.clone(),
            semantic_key: self.semantic_key.clone(),
        }
    }

    fn normalize(&mut self) -> Result<(), String> {
        self.schema = trim_or_default(&self.schema, ARTIFACT_CONTRACT_SCHEMA);
        require_schema(&self.schema, ARTIFACT_CONTRACT_SCHEMA, "artifact contract")?;
        self.kind = required_trimmed("kind", &self.kind)?;
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

impl From<ArtifactRef> for ArtifactContract {
    fn from(artifact: ArtifactRef) -> Self {
        Self {
            schema: ARTIFACT_CONTRACT_SCHEMA.to_string(),
            kind: artifact.kind,
            artifact_type: artifact.artifact_type,
            path: Some(artifact.path),
            url: artifact.url,
            public_url: artifact.public_url,
            role: artifact.role,
            label: None,
            semantic_key: artifact.semantic_key,
            size_bytes: None,
            sha256: None,
            metadata: Value::Null,
            extra: BTreeMap::new(),
        }
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
        let mut evidence: Self = serde_json::from_value(value).map_err(|err| err.to_string())?;
        evidence.normalize()?;
        Ok(evidence)
    }

    fn normalize(&mut self) -> Result<(), String> {
        self.schema = trim_or_default(&self.schema, EVIDENCE_CONTRACT_SCHEMA);
        require_schema(&self.schema, EVIDENCE_CONTRACT_SCHEMA, "evidence contract")?;
        self.kind = required_trimmed("kind", &self.kind)?;
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

fn artifact_contract_schema() -> String {
    ARTIFACT_CONTRACT_SCHEMA.to_string()
}

fn evidence_contract_schema() -> String {
    EVIDENCE_CONTRACT_SCHEMA.to_string()
}

fn default_artifact_type() -> String {
    "file".to_string()
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
}
