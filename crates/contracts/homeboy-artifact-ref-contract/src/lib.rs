//! Pure serializable artifact-reference contract types.
//!
//! `ArtifactReference`, `ArtifactRef`, `EvidenceRef`, and the reviewer-facing
//! validation describe how artifacts and evidence are addressed and serialized
//! across process boundaries. They depend only on serde and the
//! `homeboy-engine-primitives` URI codec / scheme constants, which keeps this a
//! leaf crate other crates can depend on without pulling in core.
//!
//! Conversions that couple these types to core's observation records
//! (`ArtifactRecord` -> `ArtifactRef` / `EvidenceRef`) live in `homeboy-core` as
//! free functions, so this crate stays observation-free.
//!
//! The `execution_contract` module carries the typed runtime-facing execution
//! surface (`ExecutionContract` and its `ArtifactUriContract` /
//! `LabOffloadExecutionContract` / `ApplyChangeContract` components). It lived
//! in a separate `homeboy-execution-contract` crate until that crate was merged
//! here: its whole payload is artifact-URI scheme rules built on
//! `artifact_ref`'s own scheme constants, it already depended on this crate,
//! and it had exactly one dependent (`homeboy-core`), which re-exports it.

pub mod artifact_ref;
pub mod execution_contract;

pub use artifact_ref::{
    artifact_uri, validate_reviewer_facing_artifact_ref, ArtifactRef, ArtifactReference,
    EvidenceRef, ReviewerFacingArtifactRefError, ARTIFACT_REF_SCHEMA, EVIDENCE_REF_SCHEMA,
    HOMEBOY_REF_SCHEME, METADATA_ONLY_REF_SCHEME, RUNNER_ARTIFACT_REF_SCHEME,
};
pub use execution_contract::{
    artifact_store_locator_from_runner_artifact_id, decode_uri_component,
    decode_uri_component_strict, encode_uri_component, is_remote_runner_artifact_path,
    runner_artifact_store_token, ApplyChangeContract, ArtifactUriContract, ExecutionContract,
    LabOffloadExecutionContract, EXECUTION_CONTRACT,
};
