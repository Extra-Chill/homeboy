//! helpers — extracted from claims.rs.

use glob_match::glob_match;
use regex::Regex;
use std::sync::LazyLock;


/// Check if a path looks like a MIME type (platform-agnostic, IANA standard).
pub(crate) fn is_mime_type(path: &str) -> bool {
    let lower = path.to_lowercase();
    lower.starts_with("application/")
        || lower.starts_with("text/")
        || lower.starts_with("image/")
        || lower.starts_with("audio/")
        || lower.starts_with("video/")
        || lower.starts_with("font/")
        || lower.starts_with("multipart/")
}

/// Check if a value matches any of the component's ignore patterns.
pub(crate) fn matches_ignore_pattern(value: &str, patterns: &[String]) -> bool {
    patterns.iter().any(|pattern| glob_match(pattern, value))
}

/// Check if a class name uses placeholder/example naming conventions.
pub(crate) fn is_placeholder_class(value: &str) -> bool {
    // Check each namespace segment for placeholder prefixes
    value.split('\\').any(|segment| {
        PLACEHOLDER_PREFIXES
            .iter()
            .any(|prefix| segment.starts_with(prefix))
    })
}

/// Check if a line's surrounding context suggests an example rather than a real reference.
pub(crate) fn line_suggests_example(line: &str) -> bool {
    let lower = line.to_lowercase();
    lower.contains("example")
        || lower.contains("e.g.")
        || lower.contains("e.g.,")
        || lower.contains("for instance")
        || lower.contains("sample")
        || lower.contains("such as")
        || lower.contains("this creates")
        || lower.contains("would create")
        || lower.contains("would generate")
        || lower.contains("would produce")
        || lower.contains("would rename")
        || lower.contains("would become")
        || lower.contains("would be")
        || lower.contains("could be")
        || lower.contains("hypothetical")
        || lower.contains("imagine")
        || lower.contains("suppose")
        || lower.contains("typically:")
        || lower.contains("renaming")
}

/// Check if a backslash-separated match is part of an OS filesystem path on the line.
///
/// Looks at characters before the regex match position to detect drive letters (`C:\`),
/// or other OS path indicators that mean this isn't a namespaced class reference.
pub(crate) fn is_os_path_context(line: &str, match_start: usize) -> bool {
    // Check if there's a drive letter + colon + backslash before the match
    // e.g., "C:\Users\<username>\AppData\Roaming"
    if match_start >= 2 {
        let prefix = &line[..match_start];
        // Look for X:\ pattern anywhere before the match
        if prefix.contains(":\\") || prefix.contains(":/") {
            return true;
        }
    }
    // Check if the line contains common OS path indicators
    let lower = line.to_lowercase();
    (lower.contains("c:\\") || lower.contains("c:/"))
        || (lower.contains("users\\") || lower.contains("users/"))
        || lower.contains("program files")
        || lower.contains("%appdata%")
        || lower.contains("$home")
}
