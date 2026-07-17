//! Artifact-reference URI schemes.
//!
//! The stable string schemes that identify how an artifact path should be
//! interpreted across process boundaries:
//!
//! - `homeboy://` — a homeboy-native artifact reference.
//! - `runner-artifact://` — an artifact held by a runner (fetched on demand).
//! - `metadata-only:` — a reference that carries only metadata, no payload.
//!
//! These are protocol constants shared by the runner/Lab/daemon execution
//! surface (`homeboy-core::artifact_ref` / `execution_contract`), the CLI command
//! contract, and the audit engine's artifact-portability detector. They live in
//! `homeboy-engine-primitives` — the slim shared base — so consumers depend on
//! the primitives layer rather than reaching up into `homeboy-core` (or each
//! other) for a handful of scheme strings and prefix checks.

/// Scheme for homeboy-native artifact references.
pub const HOMEBOY_REF_SCHEME: &str = "homeboy://";

/// Scheme for artifacts held by a runner (materialized on demand).
pub const RUNNER_ARTIFACT_REF_SCHEME: &str = "runner-artifact://";

/// Scheme for references that carry only metadata (no payload).
pub const METADATA_ONLY_REF_SCHEME: &str = "metadata-only:";

/// Whether `path` is a runner-artifact reference (`runner-artifact://…`).
pub fn is_runner_artifact_ref(path: &str) -> bool {
    path.starts_with(RUNNER_ARTIFACT_REF_SCHEME)
}

/// Whether `path` is a metadata-only reference (`metadata-only:…`).
pub fn is_metadata_only_ref(path: &str) -> bool {
    path.starts_with(METADATA_ONLY_REF_SCHEME)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_runner_artifact_refs() {
        assert!(is_runner_artifact_ref("runner-artifact://r/run/a"));
        assert!(!is_runner_artifact_ref("metadata-only:label"));
        assert!(!is_runner_artifact_ref("/abs/path"));
    }

    #[test]
    fn recognizes_metadata_only_refs() {
        assert!(is_metadata_only_ref("metadata-only:label"));
        assert!(!is_metadata_only_ref("runner-artifact://r/run/a"));
        assert!(!is_metadata_only_ref("/abs/path"));
    }
}
