//! release_state — extracted from types.rs.

use serde::Serialize;
use crate::component::Component;
use crate::error::Result;


/// Release state tracking for deployment decisions.
/// Captures git state relative to the last version tag.
#[derive(Debug, Clone, Serialize)]
pub struct ReleaseState {
    /// Number of commits since the last version tag
    pub commits_since_version: u32,
    /// Number of code commits (non-docs)
    #[serde(skip_serializing_if = "is_zero_u32")]
    pub code_commits: u32,
    /// Number of docs-only commits
    #[serde(skip_serializing_if = "is_zero_u32")]
    pub docs_only_commits: u32,
    /// Whether there are uncommitted changes in the working directory
    pub has_uncommitted_changes: bool,
    /// The baseline reference (tag or commit hash) used for comparison
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline_ref: Option<String>,
    /// Warning emitted when the detected baseline may not align with the current version
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline_warning: Option<String>,
}
