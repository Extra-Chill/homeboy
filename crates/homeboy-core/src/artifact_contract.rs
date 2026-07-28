//! Re-exports the pure artifact contract type from `homeboy-lifecycle-contract`
//! and provides the one conversion that couples it to `ArtifactRef`.
//!
//! Historically this module also carried `artifact_contract_from_record` and
//! `artifact_contract_from_ref`, plus an `EvidenceContract` re-export. All three
//! had zero call sites workspace-wide and were removed in #10310. The module
//! docs also used to claim these conversions "cannot live in the leaf contract
//! crate" because they couple to `ArtifactRecord` — but `ArtifactRecord` is
//! itself defined in `homeboy-lifecycle-contract`. The real reason the surviving
//! conversion lives here is `ArtifactRef`, which is owned by a *different* leaf
//! crate (`homeboy-artifact-ref-contract`) that lifecycle-contract does not
//! depend on.

use crate::artifact_ref::{ArtifactRef, ARTIFACT_REF_SCHEMA};

pub use homeboy_lifecycle_contract::artifact_contract::{
    ArtifactContract, ARTIFACT_CONTRACT_SCHEMA,
};

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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// `artifact_contract_to_ref` is the only surviving contract<->ref
    /// conversion. `artifact_contract_from_record` and
    /// `artifact_contract_from_ref` had zero call sites and were removed in
    /// #10310 along with `EvidenceContract`.
    #[test]
    fn artifact_contract_to_ref_carries_target_and_semantics() {
        let contract = ArtifactContract::from_value(json!({
            "kind": "transcript",
            "type": "json",
            "path": "artifacts/run.json",
            "public_url": "https://example.test/run.json",
            "role": "primary-output",
            "semantic_key": "task.transcript"
        }))
        .expect("artifact contract");

        let reference = artifact_contract_to_ref(&contract, "artifact-1", "run-1");

        assert_eq!(reference.schema, ARTIFACT_REF_SCHEMA);
        assert_eq!(reference.id, "artifact-1");
        assert_eq!(reference.run_id, "run-1");
        assert_eq!(reference.kind, "transcript");
        assert_eq!(reference.artifact_type, "json");
        assert_eq!(reference.path, "artifacts/run.json");
        assert_eq!(
            reference.public_url.as_deref(),
            Some("https://example.test/run.json")
        );
        assert_eq!(reference.role.as_deref(), Some("primary-output"));
        assert_eq!(reference.semantic_key.as_deref(), Some("task.transcript"));
        assert_eq!(
            reference.canonical_uri(),
            "homeboy://run/run-1/artifact/artifact-1"
        );
    }

    /// A contract with no `path` still yields a non-empty ref target by falling
    /// back to `url` then `public_url`.
    #[test]
    fn artifact_contract_to_ref_falls_back_to_url_for_the_ref_path() {
        let contract = ArtifactContract::from_value(json!({
            "kind": "log",
            "url": "https://example.test/build.log"
        }))
        .expect("artifact contract");

        let reference = artifact_contract_to_ref(&contract, "artifact-2", "run-2");

        assert_eq!(reference.path, "https://example.test/build.log");
        assert_eq!(reference.public_url, None);
    }
}
