//! version — extracted from version.rs.

use crate::component::Component;
use crate::engine::text;

use super::read_local_version;

/// Parse version from content using regex pattern.
/// Pattern must contain a capture group for the version string.
/// Content is trimmed to handle trailing newlines in VERSION files.
pub fn parse_version(content: &str, pattern: &str) -> Option<String> {
    text::extract_first(content, pattern)
}

/// Increment semver version or set an explicit version.
///
/// bump_type can be:
/// - "patch", "minor", or "major" — increments the corresponding semver component
/// - An explicit version string like "2.0.0" — returned as-is after validation
pub fn increment_version(version: &str, bump_type: &str) -> Option<String> {
    // If bump_type looks like a version string (contains a dot), use it directly
    if bump_type.contains('.') {
        let parts: Vec<&str> = bump_type.split('.').collect();
        if parts.len() != 3 || parts.iter().any(|p| p.parse::<u32>().is_err()) {
            return None; // Invalid version format
        }
        return Some(bump_type.to_string());
    }

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn increment_version_patch() {
        assert_eq!(
            increment_version("1.2.3", "patch"),
            Some("1.2.4".to_string())
        );
    }

    #[test]
    fn increment_version_minor() {
        assert_eq!(
            increment_version("1.2.3", "minor"),
            Some("1.3.0".to_string())
        );
    }

    #[test]
    fn increment_version_major() {
        assert_eq!(
            increment_version("1.2.3", "major"),
            Some("2.0.0".to_string())
        );
    }

    #[test]
    fn increment_version_explicit_version() {
        // Explicit version string passed as bump_type — returned as-is
        assert_eq!(
            increment_version("1.25.5", "2.0.0"),
            Some("2.0.0".to_string())
        );
        assert_eq!(
            increment_version("0.5.0", "1.0.0"),
            Some("1.0.0".to_string())
        );
    }

    #[test]
    fn increment_version_explicit_invalid() {
        // Invalid explicit versions
        assert_eq!(increment_version("1.0.0", "2.0"), None);
        assert_eq!(increment_version("1.0.0", "abc.def.ghi"), None);
    }

    #[test]
    fn increment_version_unknown_bump_type() {
        assert_eq!(increment_version("1.0.0", "huge"), None);
    }
}
