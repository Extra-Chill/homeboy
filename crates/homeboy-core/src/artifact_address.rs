use serde::{Deserialize, Serialize};

use crate::artifact_links::{classify_url_host, UrlHostClass};
use crate::artifact_ref::{artifact_uri, METADATA_ONLY_REF_SCHEME};
use crate::observation::ArtifactRecord;

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

        if crate::execution_contract::is_remote_runner_artifact_path(&artifact.path) {
            return Self::remote_runner_ref(artifact.path.clone());
        }

        if artifact.artifact_type == "remote_file" {
            return Self::metadata_only(format!("remote-artifact:{}", artifact.id));
        }

        if artifact.artifact_type == "metadata-only" {
            let value = if std::path::Path::new(&artifact.path).is_absolute() {
                format!("{METADATA_ONLY_REF_SCHEME}{}", artifact.id)
            } else {
                artifact.path.clone()
            };
            return Self::metadata_only(value);
        }

        if artifact.artifact_type == "url" {
            return Self::metadata_only(format!("unvalidated-url:{}", artifact.id));
        }

        Self::local_operator_path(artifact_uri(&artifact.run_id, &artifact.id))
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
    (classify_url_host(&url) == UrlHostClass::Public).then(|| value.to_string())
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
    fn records_classify_into_reviewer_visibility_and_redacted_values() {
        use ArtifactAddressKind::*;
        let canonical = "homeboy://run/run-1/artifact/artifact-1";
        for (artifact_type, path, kind, value, reviewer_target) in [
            (
                "file",
                "/tmp/private.json",
                LocalOperatorPath,
                canonical,
                None,
            ),
            (
                "url",
                "https://example.test/evidence",
                PublicUrl,
                "https://example.test/evidence",
                Some("https://example.test/evidence"),
            ),
            (
                "url",
                "http://localhost:8888/evidence",
                MetadataOnly,
                "unvalidated-url:artifact-1",
                None,
            ),
            (
                "remote_file",
                "runner-artifact://lab/run-1/artifact-1",
                RemoteRunnerRef,
                "runner-artifact://lab/run-1/artifact-1",
                Some("runner-artifact://lab/run-1/artifact-1"),
            ),
            (
                "remote_file",
                "/srv/private.zip",
                MetadataOnly,
                "remote-artifact:artifact-1",
                None,
            ),
            (
                "metadata-only",
                "/tmp/private.zip",
                MetadataOnly,
                "metadata-only:artifact-1",
                None,
            ),
        ] {
            let address = ArtifactAddress::from_record(&artifact(artifact_type, path));
            assert_eq!(address.kind, kind, "{artifact_type} {path}");
            assert_eq!(address.value, value, "{artifact_type} {path}");
            assert_eq!(address.reviewer_target(), reviewer_target);
            assert_eq!(address.reviewer_visible, reviewer_target.is_some());
        }
    }
}
