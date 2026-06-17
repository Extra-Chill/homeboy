use serde::{Deserialize, Serialize};

use crate::core::observation::ArtifactRecord;

pub const ARTIFACT_REF_SCHEMA: &str = "homeboy/artifact-ref/v1";
pub const EVIDENCE_REF_SCHEMA: &str = "homeboy/evidence-ref/v1";

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
