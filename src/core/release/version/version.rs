//! version — extracted from version.rs.

use crate::component::{self, Component, VersionTarget};
use crate::engine::text;


/// Parse version from content using regex pattern.
/// Pattern must contain a capture group for the version string.
/// Content is trimmed to handle trailing newlines in VERSION files.
pub fn parse_version(content: &str, pattern: &str) -> Option<String> {
    text::extract_first(content, pattern)
}

/// Increment semver version.
/// bump_type: "patch", "minor", or "major"
pub fn increment_version(version: &str, bump_type: &str) -> Option<String> {
    let parts: Vec<&str> = version.split('.').collect();
    if parts.len() != 3 {
        return None;
    }

    let major: u32 = parts[0].parse().ok()?;
    let minor: u32 = parts[1].parse().ok()?;
    let patch: u32 = parts[2].parse().ok()?;

    let (new_major, new_minor, new_patch) = match bump_type {
        "patch" => (major, minor, patch + 1),
        "minor" => (major, minor + 1, 0),
        "major" => (major + 1, 0, 0),
        _ => return None,
    };

    Some(format!("{}.{}.{}", new_major, new_minor, new_patch))
}

/// Get version string from a component's first version target.
/// Returns None if no version targets configured or version can't be read.
/// Use this for simple version checks (e.g., deploy outdated detection).
pub fn get_component_version(component: &Component) -> Option<String> {
    let target = component.version_targets.as_ref()?.first()?;
    read_local_version(&component.local_path, target)
}
