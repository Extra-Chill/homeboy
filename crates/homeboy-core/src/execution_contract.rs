//! Re-exports the typed execution-surface contract from
//! `homeboy-execution-contract` and provides the one path-touching classifier
//! that must reach the filesystem (`std::fs::metadata`), which cannot live in
//! the leaf contract crate.

pub use homeboy_execution_contract::execution_contract::{
    artifact_store_locator_from_runner_artifact_id, decode_uri_component,
    decode_uri_component_strict, encode_uri_component, is_remote_runner_artifact_path,
    runner_artifact_store_token, ApplyChangeContract, ArtifactUriContract, ExecutionContract,
    LabOffloadExecutionContract, EXECUTION_CONTRACT,
};

/// Whether an artifact path is worth reporting as retrievable evidence: a
/// retrievable runner-artifact token, a metadata-only ref, a relative path, or
/// a real local file/dir. Lives in core (not the leaf contract crate) because
/// the final branch probes the filesystem with `std::fs::metadata`.
pub fn is_reportable_artifact_evidence_path(path: &str) -> bool {
    EXECUTION_CONTRACT
        .artifacts
        .strip_runner_artifact_scheme(path)
        .is_some()
        || EXECUTION_CONTRACT.artifacts.is_metadata_only_ref(path)
        || !std::path::Path::new(path).is_absolute()
        || std::fs::metadata(path)
            .map(|metadata| metadata.is_file() || metadata.is_dir())
            .unwrap_or(false)
}

/// The path itself when it is reportable artifact evidence, else `None`.
pub fn reportable_artifact_evidence_path(path: Option<&String>) -> Option<String> {
    path.filter(|path| is_reportable_artifact_evidence_path(path))
        .cloned()
}
