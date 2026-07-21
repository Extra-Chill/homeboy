//! types — extracted from version.rs.

use homeboy_core::is_zero;
use serde::{Deserialize, Serialize};

// VersionTargetInfo and ComponentVersionSnapshot moved DOWN to
// homeboy-release-contract so core's context status mechanics can hold them in
// public struct fields without a cycle. Re-exported here so release code paths
// are unchanged.
pub use homeboy_release_contract::{ComponentVersionSnapshot, VersionTargetInfo};

/// Result of reading a component's version
#[derive(Debug, Clone, Serialize)]

pub struct ComponentVersionInfo {
    pub version: String,
    pub targets: Vec<VersionTargetInfo>,
}

/// Result of bumping a component's version
#[derive(Debug, Clone, Serialize)]

pub struct BumpResult {
    pub old_version: String,
    pub new_version: String,
    pub targets: Vec<VersionTargetInfo>,
    pub changelog_path: String,
    pub changelog_finalized: bool,
    pub changelog_changed: bool,
    /// Number of `@since` placeholder tags replaced with the new version.
    #[serde(skip_serializing_if = "is_zero")]
    pub since_tags_replaced: usize,
}

/// Result of validating and finalizing changelog for a version operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangelogValidationResult {
    pub changelog_path: String,
    pub changelog_finalized: bool,
    pub changelog_changed: bool,
}

/// Detect version targets in a directory by checking for well-known version files.
/// Information about a version pattern found but not configured
#[derive(Debug, Clone, Serialize)]
pub struct UnconfiguredPattern {
    pub file: String,
    pub pattern: String,
    pub description: String,
    pub found_version: String,
    pub full_path: String,
}

/// Default placeholder pattern for `@since` tags.
pub(crate) const DEFAULT_SINCE_PLACEHOLDER: &str = r"0\.0\.0|NEXT|TBD|TODO|UNRELEASED|x\.x\.x";
