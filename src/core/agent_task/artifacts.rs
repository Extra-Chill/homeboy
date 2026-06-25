use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::schema::artifact_schema;
use crate::core::redaction::RedactionPolicy;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskArtifactDeclaration {
    pub name: String,
    #[serde(
        default,
        rename = "type",
        alias = "artifact_type",
        alias = "artifactType",
        alias = "kind",
        skip_serializing_if = "Option::is_none"
    )]
    pub artifact_type: Option<String>,
    #[serde(
        default,
        alias = "artifactSchema",
        alias = "content_schema",
        alias = "contentSchema",
        skip_serializing_if = "Option::is_none"
    )]
    pub artifact_schema: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default)]
    pub required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
}

impl AgentTaskArtifactDeclaration {
    pub(super) fn canonical(&self) -> Option<Self> {
        let name = non_empty_trimmed(&self.name)?;
        Some(Self {
            name,
            artifact_type: self.artifact_type.as_deref().and_then(non_empty_trimmed),
            artifact_schema: self.artifact_schema.as_deref().and_then(non_empty_trimmed),
            path: self.path.as_deref().and_then(non_empty_trimmed),
            required: self.required,
            description: self.description.as_deref().and_then(non_empty_trimmed),
            metadata: self.metadata.clone(),
        })
    }

    pub(super) fn from_expected_artifact(expected: &str) -> Option<Self> {
        let name = non_empty_trimmed(expected)?;
        Some(Self {
            name,
            artifact_type: None,
            artifact_schema: None,
            path: None,
            required: true,
            description: None,
            metadata: Value::Null,
        })
    }
}

fn non_empty_trimmed(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskTypedArtifact {
    pub name: String,
    #[serde(default, rename = "type", skip_serializing_if = "Option::is_none")]
    pub artifact_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_schema: Option<String>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub payload: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact: Option<AgentTaskArtifact>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
}

#[cfg(test)]
impl AgentTaskArtifactDeclaration {
    pub(super) fn redacted_with(mut self, policy: &RedactionPolicy) -> Self {
        self.description = self.description.map(|value| policy.redact_string(&value));
        self.path = self.path.map(|value| policy.redact_string(&value));
        self.metadata = policy.redact_json(&self.metadata);
        self
    }
}

#[cfg(test)]
impl AgentTaskTypedArtifact {
    pub(super) fn redacted_with(mut self, policy: &RedactionPolicy) -> Self {
        self.payload = policy.redact_json(&self.payload);
        self.artifact = self.artifact.map(|artifact| artifact.redacted_with(policy));
        self.metadata = policy.redact_json(&self.metadata);
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskArtifact {
    #[serde(default = "artifact_schema")]
    pub schema: String,
    pub id: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
}

impl AgentTaskArtifact {
    pub fn display_label(&self) -> Option<&str> {
        self.label
            .as_deref()
            .or(self.name.as_deref())
            .or(Some(self.id.as_str()))
    }

    pub fn declared_role(&self) -> Option<&str> {
        self.role.as_deref().or_else(|| {
            self.metadata
                .get("role")
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
        })
    }

    pub fn declared_semantic_key(&self) -> Option<&str> {
        self.semantic_key.as_deref().or_else(|| {
            self.metadata
                .get("semantic_key")
                .or_else(|| self.metadata.get("semanticKey"))
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
        })
    }
}

#[cfg(test)]
impl AgentTaskArtifact {
    pub(super) fn redacted_with(mut self, policy: &RedactionPolicy) -> Self {
        self.name = self.name.map(|value| policy.redact_string(&value));
        self.label = self.label.map(|value| policy.redact_string(&value));
        self.role = self.role.map(|value| policy.redact_string(&value));
        self.semantic_key = self.semantic_key.map(|value| policy.redact_string(&value));
        self.path = self.path.map(|value| policy.redact_string(&value));
        self.url = self.url.map(|value| policy.redact_url(&value));
        self.metadata = policy.redact_json(&self.metadata);
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskEvidenceRef {
    pub kind: String,
    pub uri: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskDiagnostic {
    pub class: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub data: Value,
}

impl AgentTaskDiagnostic {
    pub(super) fn redacted_with(mut self, policy: &RedactionPolicy) -> Self {
        self.message = policy.redact_string(&self.message);
        self.data = policy.redact_json(&self.data);
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskFollowUp {
    pub kind: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
}
