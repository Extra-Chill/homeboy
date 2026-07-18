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

pub mod artifact_ref;

pub use artifact_ref::{
    artifact_uri, validate_reviewer_facing_artifact_ref, ArtifactRef, ArtifactReference,
    EvidenceRef, ReviewerFacingArtifactRefError, ARTIFACT_REF_SCHEMA, EVIDENCE_REF_SCHEMA,
    HOMEBOY_REF_SCHEME, METADATA_ONLY_REF_SCHEME, RUNNER_ARTIFACT_REF_SCHEME,
};
