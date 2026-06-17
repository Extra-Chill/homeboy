use serde::{Deserialize, Serialize};

use crate::core::execution_contract::encode_uri_component;
use crate::core::observation::ArtifactRecord;

pub const ARTIFACT_ADDRESS_SCHEMA: &str = "homeboy/artifact-address/v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArtifactAddress {
    pub schema: String,
    pub kind: ArtifactAddressKind,
    pub value: String,
    pub reviewer_visible: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub validation: Option<ArtifactAddressValidation>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactAddressKind {
    LocalOperatorPath,
    RemoteRunnerRef,
    PublicUrl,
    MetadataOnly,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArtifactAddressValidation {
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl ArtifactAddress {
    pub fn from_record(artifact: &ArtifactRecord) -> Self {
        if let Some(public_url) = public_url_for_record(artifact) {
            return Self::public_url(public_url);
        }

        if crate::core::runners::is_remote_runner_artifact_path(&artifact.path) {
            return Self::remote_runner_ref(artifact.path.clone());
        }

        if artifact.artifact_type == "remote_file" {
            return Self::metadata_only(format!("remote-artifact:{}", artifact.id));
        }

        if artifact.artifact_type == "metadata-only" {
            let value = if std::path::Path::new(&artifact.path).is_absolute() {
                format!("metadata-only:{}", artifact.id)
            } else {
                artifact.path.clone()
            };
            return Self::metadata_only(value);
        }

        if artifact.artifact_type == "url" {
            return Self::metadata_only(format!("unvalidated-url:{}", artifact.id));
        }

        Self::local_operator_path(format!(
            "homeboy://run/{}/artifact/{}",
            encode_uri_component(&artifact.run_id),
            encode_uri_component(&artifact.id)
        ))
    }

    pub fn reviewer_target(&self) -> Option<&str> {
        self.reviewer_visible.then_some(self.value.as_str())
    }

    fn public_url(url: String) -> Self {
        Self {
            schema: ARTIFACT_ADDRESS_SCHEMA.to_string(),
            kind: ArtifactAddressKind::PublicUrl,
            value: url,
            reviewer_visible: true,
            validation: Some(ArtifactAddressValidation {
                status: "validated".to_string(),
                reason: None,
            }),
        }
    }

    fn remote_runner_ref(value: String) -> Self {
        Self {
            schema: ARTIFACT_ADDRESS_SCHEMA.to_string(),
            kind: ArtifactAddressKind::RemoteRunnerRef,
            value,
            reviewer_visible: true,
            validation: None,
        }
    }

    fn metadata_only(value: String) -> Self {
        Self {
            schema: ARTIFACT_ADDRESS_SCHEMA.to_string(),
            kind: ArtifactAddressKind::MetadataOnly,
            value,
            reviewer_visible: false,
            validation: None,
        }
    }

    fn local_operator_path(value: String) -> Self {
        Self {
            schema: ARTIFACT_ADDRESS_SCHEMA.to_string(),
            kind: ArtifactAddressKind::LocalOperatorPath,
            value,
            reviewer_visible: false,
            validation: Some(ArtifactAddressValidation {
                status: "local_only".to_string(),
                reason: Some(
                    "local operator paths are not reviewer-facing evidence targets".to_string(),
                ),
            }),
        }
    }
}

pub fn validated_public_url(value: &str) -> Option<String> {
    let url = reqwest::Url::parse(value).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    let host = url.host_str()?.to_ascii_lowercase();
    let local_host =
        host == "localhost" || host == "127.0.0.1" || host == "::1" || host.ends_with(".local");
    (!local_host).then(|| value.to_string())
}

fn public_url_for_record(artifact: &ArtifactRecord) -> Option<String> {
    artifact
        .public_url
        .as_deref()
        .or(artifact.url.as_deref())
        .or_else(|| {
            artifact
                .metadata_json
                .get("public_url")
                .and_then(|value| value.as_str())
        })
        .or_else(|| (artifact.artifact_type == "url").then_some(artifact.path.as_str()))
        .and_then(validated_public_url)
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::*;

    fn artifact(artifact_type: &str, path: &str) -> ArtifactRecord {
        ArtifactRecord {
            id: "artifact-1".to_string(),
            run_id: "run-1".to_string(),
            kind: "trace".to_string(),
            artifact_type: artifact_type.to_string(),
            path: path.to_string(),
            url: None,
            public_url: None,
            viewer_url: None,
            viewer_links: Vec::new(),
            sha256: None,
            size_bytes: None,
            mime: None,
            metadata_json: Value::Null,
            created_at: "2026-06-12T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn local_paths_become_non_reviewer_visible_artifact_refs() {
        let address = ArtifactAddress::from_record(&artifact("file", "/tmp/private.json"));

        assert_eq!(address.kind, ArtifactAddressKind::LocalOperatorPath);
        assert_eq!(address.value, "homeboy://run/run-1/artifact/artifact-1");
        assert!(!address.reviewer_visible);
        assert_eq!(address.reviewer_target(), None);
    }

    #[test]
    fn public_urls_are_reviewer_visible_after_validation() {
        let address =
            ArtifactAddress::from_record(&artifact("url", "https://example.test/evidence"));

        assert_eq!(address.kind, ArtifactAddressKind::PublicUrl);
        assert_eq!(
            address.reviewer_target(),
            Some("https://example.test/evidence")
        );
    }

    #[test]
    fn localhost_urls_are_not_reviewer_visible_public_urls() {
        let address =
            ArtifactAddress::from_record(&artifact("url", "http://localhost:8888/evidence"));

        assert_eq!(address.kind, ArtifactAddressKind::MetadataOnly);
        assert!(!address.reviewer_visible);
    }

    #[test]
    fn remote_runner_refs_are_reviewer_visible_tokens() {
        let address = ArtifactAddress::from_record(&artifact(
            "remote_file",
            "runner-artifact://lab/run-1/artifact-1",
        ));

        assert_eq!(address.kind, ArtifactAddressKind::RemoteRunnerRef);
        assert_eq!(
            address.reviewer_target(),
            Some("runner-artifact://lab/run-1/artifact-1")
        );
    }

    #[test]
    fn malformed_remote_file_paths_are_metadata_only() {
        let address = ArtifactAddress::from_record(&artifact("remote_file", "/srv/private.zip"));

        assert_eq!(address.kind, ArtifactAddressKind::MetadataOnly);
        assert_eq!(address.value, "remote-artifact:artifact-1");
        assert!(!address.reviewer_visible);
    }

    #[test]
    fn absolute_metadata_only_paths_are_redacted_to_labels() {
        let address = ArtifactAddress::from_record(&artifact("metadata-only", "/tmp/private.zip"));

        assert_eq!(address.kind, ArtifactAddressKind::MetadataOnly);
        assert_eq!(address.value, "metadata-only:artifact-1");
        assert!(!address.reviewer_visible);
    }
}
