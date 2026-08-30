//! Transport-neutral runner artifact and mutation-result contracts.

use serde::{Deserialize, Serialize};

/// A reference to an artifact produced by a runner job.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunnerArtifactRef {
    pub artifact_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport: Option<String>,
}

/// Artifact references produced by a runner mutation.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunnerMutationArtifacts {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub patch_ref: Option<RunnerArtifactRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_bundle_ref: Option<RunnerArtifactRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_log_ref: Option<RunnerArtifactRef>,
}

impl RunnerMutationArtifacts {
    pub fn is_empty(&self) -> bool {
        self.patch_ref.is_none()
            && self.file_bundle_ref.is_none()
            && self.operation_log_ref.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn artifact() -> RunnerArtifactRef {
        RunnerArtifactRef {
            artifact_id: "patch".to_string(),
            name: Some("changes.patch".to_string()),
            path: Some("artifacts/changes.patch".to_string()),
            url: Some("https://example.test/changes.patch".to_string()),
            mime: Some("text/x-diff".to_string()),
            size_bytes: Some(42),
            sha256: Some("abc123".to_string()),
            transport: Some("runner-artifact".to_string()),
        }
    }

    #[test]
    fn artifact_ref_keeps_its_minimal_wire_shape() {
        let value = serde_json::json!({ "artifact_id": "patch" });
        let minimal = RunnerArtifactRef {
            artifact_id: "patch".to_string(),
            name: None,
            path: None,
            url: None,
            mime: None,
            size_bytes: None,
            sha256: None,
            transport: None,
        };

        assert_eq!(serde_json::to_value(&minimal).expect("serialize"), value);
        assert_eq!(
            serde_json::from_value::<RunnerArtifactRef>(value).expect("deserialize"),
            minimal
        );
    }

    #[test]
    fn artifact_ref_keeps_its_complete_wire_shape() {
        assert_eq!(
            serde_json::to_value(artifact()).expect("serialize"),
            serde_json::json!({
                "artifact_id": "patch",
                "name": "changes.patch",
                "path": "artifacts/changes.patch",
                "url": "https://example.test/changes.patch",
                "mime": "text/x-diff",
                "size_bytes": 42,
                "sha256": "abc123",
                "transport": "runner-artifact",
            })
        );
    }

    #[test]
    fn mutation_artifacts_omit_absent_refs() {
        let empty = RunnerMutationArtifacts::default();
        assert!(empty.is_empty());
        assert_eq!(
            serde_json::to_value(&empty).expect("serialize"),
            serde_json::json!({})
        );
        assert_eq!(
            serde_json::from_value::<RunnerMutationArtifacts>(serde_json::json!({}))
                .expect("deserialize"),
            empty
        );

        let mutation = RunnerMutationArtifacts {
            patch_ref: Some(artifact()),
            ..Default::default()
        };
        assert!(!mutation.is_empty());
        let value = serde_json::to_value(mutation).expect("serialize");
        assert_eq!(value["patch_ref"]["artifact_id"], "patch");
        assert!(value.get("file_bundle_ref").is_none());
        assert!(value.get("operation_log_ref").is_none());
    }
}
