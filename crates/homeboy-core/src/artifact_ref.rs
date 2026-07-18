//! Re-exports the pure artifact-reference contract types from
//! `homeboy-artifact-ref-contract` and provides the conversions that couple them
//! to core's observation records (`ArtifactRecord`), which cannot live in the
//! leaf contract crate.

use crate::observation::ArtifactRecord;

pub use homeboy_artifact_ref_contract::artifact_ref::{
    artifact_uri, validate_reviewer_facing_artifact_ref, ArtifactRef, ArtifactReference,
    EvidenceRef, ReviewerFacingArtifactRefError, ARTIFACT_REF_SCHEMA, EVIDENCE_REF_SCHEMA,
    HOMEBOY_REF_SCHEME, METADATA_ONLY_REF_SCHEME, RUNNER_ARTIFACT_REF_SCHEME,
};

/// Build an [`ArtifactRef`] from an observation [`ArtifactRecord`].
///
/// Lives in core (not the contract crate) because it couples the pure ref shape
/// to core's observation record type.
pub fn artifact_ref_from_record(artifact: &ArtifactRecord) -> ArtifactRef {
    ArtifactRef {
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

/// Build an [`EvidenceRef`] pointing at an observation [`ArtifactRecord`].
///
/// Lives in core (not the contract crate) because it couples the pure evidence
/// shape to core's observation record type.
pub fn evidence_ref_for_artifact(
    artifact: &ArtifactRecord,
    label: impl Into<String>,
    role: Option<String>,
    semantic_key: Option<String>,
) -> EvidenceRef {
    let mut artifact_ref = artifact_ref_from_record(artifact);
    artifact_ref.role = role.clone();
    artifact_ref.semantic_key = semantic_key.clone();
    EvidenceRef {
        schema: EVIDENCE_REF_SCHEMA.to_string(),
        kind: "artifact".to_string(),
        target: artifact_ref.canonical_uri(),
        label: label.into(),
        role,
        semantic_key,
        artifact: Some(artifact_ref),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn evidence_ref_builds_generic_homeboy_artifact_uri() {
        let artifact = ArtifactRecord {
            id: "artifact 1".to_string(),
            run_id: "run/1".to_string(),
            kind: "fuzz_result_envelope".to_string(),
            artifact_type: "file".to_string(),
            path: "summary.json".to_string(),
            url: None,
            public_url: None,
            viewer_url: None,
            viewer_links: Vec::new(),
            sha256: None,
            size_bytes: None,
            mime: None,
            metadata_json: json!({}),
            created_at: "2026-06-27T00:00:00Z".to_string(),
        };

        let reference = evidence_ref_for_artifact(
            &artifact,
            "Fuzz result envelope",
            Some("result".to_string()),
            Some("fuzz.result_envelope".to_string()),
        );

        assert_eq!(
            reference.canonical_uri(),
            "homeboy://run/run%2F1/artifact/artifact%201"
        );
        assert_eq!(reference.kind, "artifact");
        assert_eq!(
            reference.artifact.expect("artifact").role.as_deref(),
            Some("result")
        );
    }
}
