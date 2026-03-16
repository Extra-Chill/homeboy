//! types — extracted from version.rs.

use crate::error::{Error, Result};
use crate::is_zero;
use crate::release::changelog;
use serde::Serialize;
use crate::core::release::*;


/// Information about a version target after reading
#[derive(Debug, Clone, Serialize)]

pub struct VersionTargetInfo {
    pub file: String,
    pub pattern: String,
    pub full_path: String,
    pub match_count: usize,
}

/// Result of reading a component's version
#[derive(Debug, Clone, Serialize)]

pub struct ComponentVersionInfo {
    pub version: String,
    pub targets: Vec<VersionTargetInfo>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ComponentVersionSnapshot {
    pub component_id: String,
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
#[derive(Debug, Clone, Serialize)]
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
