//! line_suggests_real — extracted from claims.rs.

use regex::Regex;
use std::sync::LazyLock;
use super::ClaimConfidence;


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

/// Check if a line's context suggests a real reference (annotation, cross-ref).
pub(crate) fn line_suggests_real(line: &str) -> bool {
    line.contains("@see")
        || line.contains("@uses")
        || line.contains("@link")
        || line.contains("@param")
        || line.contains("@return")
        || line.contains("@throws")
}

/// Classify confidence for a file/directory path claim.
pub(crate) fn classify_path_confidence(value: &str, line: &str, in_code_block: bool) -> ClaimConfidence {
    if in_code_block {
        return ClaimConfidence::Example;
    }
    if line_suggests_real(line) {
        return ClaimConfidence::Real;
    }
    if line_suggests_example(line) {
        return ClaimConfidence::Example;
    }
    // Path references in prose default to real — they should resolve
    let lower = value.to_lowercase();
    if lower.contains("example") || lower.contains("sample") || lower.contains("your-") {
        return ClaimConfidence::Example;
    }
    ClaimConfidence::Real
}

/// Classify confidence for a class name claim.
pub(crate) fn classify_class_confidence(value: &str, line: &str, in_code_block: bool) -> ClaimConfidence {
    if is_placeholder_class(value) {
        return ClaimConfidence::Example;
    }
    if in_code_block {
        // Inside a code block with a non-placeholder name — could be real or example
        if line_suggests_real(line) {
            return ClaimConfidence::Real;
        }
        return ClaimConfidence::Unclear;
    }
    if line_suggests_example(line) {
        return ClaimConfidence::Example;
    }
    ClaimConfidence::Real
}
