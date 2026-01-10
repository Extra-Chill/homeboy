use regex::Regex;

/// Parse version from content using regex pattern.
/// Pattern must contain a capture group for the version string.
pub fn parse_version(content: &str, pattern: &str) -> Option<String> {
    let re = Regex::new(pattern).ok()?;
    re.captures(content)
        .and_then(|caps| caps.get(1))
        .map(|m| m.as_str().to_string())
}

/// Get default version pattern based on file extension.
pub fn default_pattern_for_file(filename: &str) -> &'static str {
    if filename.ends_with(".toml") {
        r#"version\s*=\s*"(\d+\.\d+\.\d+)""#
    } else if filename.ends_with(".json") {
        r#""version"\s*:\s*"(\d+\.\d+\.\d+)""#
    } else if filename.ends_with(".php") {
        r"Version:\s*(\d+\.\d+\.\d+)"
    } else {
        r"(\d+\.\d+\.\d+)"
    }
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
