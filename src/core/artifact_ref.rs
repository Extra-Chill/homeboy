use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::core::execution_contract::decode_uri_component;
use crate::core::observation::ArtifactRecord;

pub const ARTIFACT_REF_SCHEMA: &str = "homeboy/artifact-ref/v1";
pub const EVIDENCE_REF_SCHEMA: &str = "homeboy/evidence-ref/v1";
pub const RUNNER_ARTIFACT_REF_SCHEME: &str = "runner-artifact://";
pub const METADATA_ONLY_REF_SCHEME: &str = "metadata-only:";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArtifactReference {
    LocalPath(String),
    RunnerArtifact {
        value: String,
        runner_id: String,
        run_id: String,
        artifact_id: String,
    },
    MetadataOnly(String),
    PublishedUrl(String),
}

impl ArtifactReference {
    pub fn parse(value: impl Into<String>) -> Self {
        let value = value.into();
        if let Some(rest) = value.strip_prefix(RUNNER_ARTIFACT_REF_SCHEME) {
            let parts = rest.split('/').collect::<Vec<_>>();
            if parts.len() == 3 {
                let runner_id = decode_uri_component(parts[0]);
                let run_id = decode_uri_component(parts[1]);
                let artifact_id = decode_uri_component(parts[2]);
                return Self::RunnerArtifact {
                    value,
                    runner_id,
                    run_id,
                    artifact_id,
                };
            }
        }

        if value.starts_with(METADATA_ONLY_REF_SCHEME) {
            return Self::MetadataOnly(value);
        }

        if is_published_url(&value) {
            return Self::PublishedUrl(value);
        }

        Self::LocalPath(value)
    }

    pub fn metadata_only(label: &str) -> Self {
        Self::MetadataOnly(format!("{}{label}", METADATA_ONLY_REF_SCHEME))
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::LocalPath(value)
            | Self::MetadataOnly(value)
            | Self::PublishedUrl(value)
            | Self::RunnerArtifact { value, .. } => value,
        }
    }
}

impl fmt::Display for ArtifactReference {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl Serialize for ArtifactReference {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ArtifactReference {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Ok(Self::parse(value))
    }
}

fn is_published_url(value: &str) -> bool {
    value.starts_with("https://") || value.starts_with("http://")
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArtifactRef {
    pub schema: String,
    pub id: String,
    pub run_id: String,
    pub kind: String,
    #[serde(rename = "type")]
    pub artifact_type: String,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_key: Option<String>,
}

impl ArtifactRef {
    pub fn from_record(artifact: &ArtifactRecord) -> Self {
        Self {
            schema: ARTIFACT_REF_SCHEMA.to_string(),
            id: artifact.id.clone(),
            run_id: artifact.run_id.clone(),
            kind: artifact.kind.clone(),
            artifact_type: artifact.artifact_type.clone(),
            path: artifact.path.clone(),
            url: artifact.url.clone(),
            public_url: artifact.public_url.clone(),
            role: None,
            semantic_key: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EvidenceRef {
    pub schema: String,
    pub kind: String,
    pub target: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact: Option<ArtifactRef>,
}

impl EvidenceRef {
    pub fn new(
        kind: impl Into<String>,
        target: impl Into<String>,
        label: impl Into<String>,
    ) -> Self {
        Self {
            schema: EVIDENCE_REF_SCHEMA.to_string(),
            kind: kind.into(),
            target: target.into(),
            label: label.into(),
            role: None,
            semantic_key: None,
            artifact: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn artifact_reference_parses_and_serializes_existing_string_refs() {
        let runner = ArtifactReference::parse("runner-artifact://runner%2Fa/run%20b/artifact%3Ac");
        assert_eq!(
            runner.to_string(),
            "runner-artifact://runner%2Fa/run%20b/artifact%3Ac"
        );
        assert_eq!(
            serde_json::to_value(&runner).expect("json"),
            json!("runner-artifact://runner%2Fa/run%20b/artifact%3Ac")
        );
        match runner {
            ArtifactReference::RunnerArtifact {
                runner_id,
                run_id,
                artifact_id,
                ..
            } => {
                assert_eq!(runner_id, "runner/a");
                assert_eq!(run_id, "run b");
                assert_eq!(artifact_id, "artifact:c");
            }
            other => panic!("unexpected artifact reference: {other:?}"),
        }

        assert_eq!(
            ArtifactReference::metadata_only("trace.zip").to_string(),
            "metadata-only:trace.zip"
        );
        assert_eq!(
            ArtifactReference::parse("https://example.test/trace.zip"),
            ArtifactReference::PublishedUrl("https://example.test/trace.zip".to_string())
        );
        assert_eq!(
            ArtifactReference::parse("/tmp/trace.zip"),
            ArtifactReference::LocalPath("/tmp/trace.zip".to_string())
        );
    }

    #[test]
    fn artifact_ref_serializes_stable_schema_and_type_field() {
        let artifact = ArtifactRef {
            schema: ARTIFACT_REF_SCHEMA.to_string(),
            id: "artifact-1".to_string(),
            run_id: "run-1".to_string(),
            kind: "summary".to_string(),
            artifact_type: "file".to_string(),
            path: "summary.json".to_string(),
            url: None,
            public_url: Some("https://example.test/summary.json".to_string()),
            role: Some("summary".to_string()),
            semantic_key: Some("run.summary".to_string()),
        };

        assert_eq!(
            serde_json::to_value(&artifact).expect("artifact ref json"),
            json!({
                "schema": "homeboy/artifact-ref/v1",
                "id": "artifact-1",
                "run_id": "run-1",
                "kind": "summary",
                "type": "file",
                "path": "summary.json",
                "public_url": "https://example.test/summary.json",
                "role": "summary",
                "semantic_key": "run.summary"
            })
        );
    }
}
