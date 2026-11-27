//! Re-exports the pure artifact contract type from `homeboy-lifecycle-contract`
//! and provides the one conversion that couples it to `ArtifactRef`.
//!
//! The conversion lives here rather than in the leaf contract crate because
//! `ArtifactRef` is owned by core's `artifact_ref` module, which
//! lifecycle-contract does not depend on.

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

    /// The ref target prefers `path`, then `url`, then `public_url`, so a
    /// contract without a path still yields a non-empty ref target.
    #[test]
    fn artifact_contract_to_ref_falls_back_through_path_url_and_public_url() {
        for (contract, expected_path) in [
            (
                json!({
                    "kind": "transcript",
                    "path": "artifacts/run.json",
                    "url": "https://example.test/raw.json",
                    "public_url": "https://example.test/run.json"
                }),
                "artifacts/run.json",
            ),
            (
                json!({
                    "kind": "log",
                    "url": "https://example.test/build.log",
                    "public_url": "https://example.test/public.log"
                }),
                "https://example.test/build.log",
            ),
            (
                json!({ "kind": "log", "public_url": "https://example.test/public.log" }),
                "https://example.test/public.log",
            ),
        ] {
            let contract = ArtifactContract::from_value(contract).expect("artifact contract");
            let reference = artifact_contract_to_ref(&contract, "artifact-1", "run-1");
            assert_eq!(reference.path, expected_path);
        }
    }
}
