//! Re-exports the pure artifact/evidence contract types from
//! `homeboy-lifecycle-contract` and provides the conversions that couple them
//! to core's observation records and `ArtifactRef` (which cannot live in the
//! leaf contract crate).

use std::collections::BTreeMap;

use serde_json::Value;

use crate::artifact_ref::{ArtifactRef, ARTIFACT_REF_SCHEMA};
use crate::observation::ArtifactRecord;

pub use homeboy_lifecycle_contract::artifact_contract::{
    ArtifactContract, EvidenceContract, ARTIFACT_CONTRACT_SCHEMA, EVIDENCE_CONTRACT_SCHEMA,
};

/// Build an [`ArtifactContract`] from an observation [`ArtifactRecord`].
pub fn artifact_contract_from_record(record: &ArtifactRecord) -> ArtifactContract {
    ArtifactContract {
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

/// Convert an [`ArtifactContract`] into an [`ArtifactRef`].
pub fn artifact_contract_to_ref(
    contract: &ArtifactContract,
    id: impl Into<String>,
    run_id: impl Into<String>,
) -> ArtifactRef {
    ArtifactRef {
        schema: ARTIFACT_REF_SCHEMA.to_string(),
        id: id.into(),
        run_id: run_id.into(),
        kind: contract.kind.clone(),
        artifact_type: contract.artifact_type.clone(),
        path: contract
            .path
            .clone()
            .or_else(|| contract.url.clone())
            .or_else(|| contract.public_url.clone())
            .unwrap_or_default(),
        url: contract.url.clone(),
        public_url: contract.public_url.clone(),
        role: contract.role.clone(),
        semantic_key: contract.semantic_key.clone(),
    }
}

/// Build an [`ArtifactContract`] from an [`ArtifactRef`].
pub fn artifact_contract_from_ref(artifact: ArtifactRef) -> ArtifactContract {
    ArtifactContract {
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
