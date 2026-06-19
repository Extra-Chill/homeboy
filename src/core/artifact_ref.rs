use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::core::execution_contract::{decode_uri_component, encode_uri_component};
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
                return Self::RunnerArtifact {
                    value,
                    runner_id: decode_uri_component(parts[0]),
                    run_id: decode_uri_component(parts[1]),
                    artifact_id: decode_uri_component(parts[2]),
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

    pub fn runner_artifact(runner_id: &str, run_id: &str, artifact_id: &str) -> Self {
        Self::parse(format!(
            "{}{}/{}/{}",
            RUNNER_ARTIFACT_REF_SCHEME,
            encode_uri_component(runner_id),
            encode_uri_component(run_id),
            encode_uri_component(artifact_id)
        ))
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
        }
    }

    pub fn public_target(&self) -> Option<String> {
        self.public_url
            .clone()
            .or_else(|| self.url.clone())
            .or_else(|| (self.artifact_type == "url").then(|| self.path.clone()))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EvidenceRef {
    pub schema: String,
    pub kind: String,
    pub target: String,
    pub label: String,
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
            artifact: None,
        }
    }

    pub fn from_artifact(artifact: ArtifactRef) -> Option<Self> {
        let target = artifact.public_target()?;
        Some(Self {
            schema: EVIDENCE_REF_SCHEMA.to_string(),
            kind: artifact.kind.clone(),
            target,
            label: artifact.kind.clone(),
            artifact: Some(artifact),
        })
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
                "public_url": "https://example.test/summary.json"
            })
        );
    }

    #[test]
    fn evidence_ref_can_wrap_public_artifact_ref() {
        let artifact = ArtifactRef {
            schema: ARTIFACT_REF_SCHEMA.to_string(),
            id: "artifact-1".to_string(),
            run_id: "run-1".to_string(),
            kind: "review".to_string(),
            artifact_type: "url".to_string(),
            path: "https://example.test/review".to_string(),
            url: None,
            public_url: None,
        };

        let evidence = EvidenceRef::from_artifact(artifact).expect("evidence ref");
        assert_eq!(evidence.schema, EVIDENCE_REF_SCHEMA);
        assert_eq!(evidence.target, "https://example.test/review");
        assert_eq!(
            evidence.artifact.expect("artifact").schema,
            ARTIFACT_REF_SCHEMA
        );
    }
}
