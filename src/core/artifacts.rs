//! Stable facade for artifact contracts, links, manifests, and publication helpers.
//!
//! New command/core code should import artifact APIs from this module instead of
//! reaching into individual artifact implementation modules.

pub use super::artifact_inputs::*;
pub use super::artifact_links::*;
pub use super::artifact_manifest::*;
pub use super::artifact_origin::*;
pub use super::browser_evidence::*;
pub use super::change_artifact::*;
pub use super::publication_artifacts::*;
pub use super::structured_sidecar::*;

/// Resolve the artifact root used for copied/downloaded run artifacts.
pub fn root() -> super::Result<std::path::PathBuf> {
    super::artifact_root()
}
