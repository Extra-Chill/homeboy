//! content_search_boundary — extracted from codebase_scan.rs.

use std::path::{Path, PathBuf};
use super::ScanConfig;


/// Check if a directory should be skipped based on config.
///
/// Centralizes the skip-dir decision for all walker variants.
pub(crate) fn should_skip_dir(name: &str, is_root: bool, config: &ScanConfig) -> bool {
    // Skip hidden directories if configured
    if config.skip_hidden && name.starts_with('.') {
        return true;
    }

    // Always skip VCS/dependency dirs at any depth
    if ALWAYS_SKIP_DIRS.contains(&name) {
        return true;
    }
    if config.extra_skip_dirs.iter().any(|d| d.as_str() == name) {
        return true;
    }

    // Skip build output dirs only at root level
    if is_root {
        if ROOT_ONLY_SKIP_DIRS.contains(&name) {
            return true;
        }
        if config
            .extra_root_skip_dirs
            .iter()
            .any(|d| d.as_str() == name)
        {
            return true;
        }
    }

    false
}

/// Check if a byte is a word boundary character (not alphanumeric, not underscore).
pub(crate) fn is_boundary_char(c: u8) -> bool {
    !c.is_ascii_alphanumeric() && c != b'_'
}

/// Find all occurrences of `term` in `text` at sensible word boundaries.
///
/// Boundary rules:
/// - Left: start of string, non-alphanumeric, underscore, or camelCase/acronym boundary
/// - Right: end of string, non-alphanumeric, underscore, or uppercase letter
///
/// Handles: word boundaries, camelCase joins, snake_case compounds, UPPER_SNAKE,
/// consecutive-uppercase acronym boundaries (WPAgent → WP|Agent).
pub fn find_boundary_matches(text: &str, term: &str) -> Vec<usize> {
    let text_bytes = text.as_bytes();
    let term_bytes = term.as_bytes();
    let term_len = term_bytes.len();
    let text_len = text_bytes.len();
    let mut matches = Vec::new();

    if term_len == 0 || term_len > text_len {
        return matches;
    }

    let mut start = 0;
    while let Some(pos) = text[start..].find(term) {
        let abs = start + pos;
        let end = abs + term_len;

        // Left boundary
        let left_ok = abs == 0
            || is_boundary_char(text_bytes[abs - 1])
            || text_bytes[abs - 1] == b'_'
            // camelCase boundary: lowercase/digit → uppercase
            || (text_bytes[abs].is_ascii_uppercase()
                && (text_bytes[abs - 1].is_ascii_lowercase()
                    || text_bytes[abs - 1].is_ascii_digit()))
            // Consecutive-uppercase boundary: uppercase → uppercase+lowercase
            // e.g., 'P' before 'A' in "WPAgent"
            || (abs >= 2
                && text_bytes[abs].is_ascii_uppercase()
                && text_bytes[abs - 1].is_ascii_uppercase()
                && term_len > 1
                && term_bytes[1].is_ascii_lowercase());

        // Right boundary
        let right_ok = end >= text_len || {
            let next = text_bytes[end];
            is_boundary_char(next) || next.is_ascii_uppercase() || next == b'_'
        };

        if left_ok && right_ok {
            matches.push(abs);
        }

        start = abs + 1;
    }

    matches
}

/// Find all occurrences of `term` in `text` using exact substring matching.
/// No boundary detection — every occurrence is returned.
pub fn find_literal_matches(text: &str, term: &str) -> Vec<usize> {
    let mut matches = Vec::new();
    let term_len = term.len();
    if term_len == 0 {
        return matches;
    }
    let mut start = 0;
    while let Some(pos) = text[start..].find(term) {
        let abs = start + pos;
        matches.push(abs);
        start = abs + 1;
    }
    matches
}

/// Find all occurrences of `term` in `text` using case-insensitive matching,
/// returning the actual text that was found (preserving original casing).
///
/// This is used for variant discovery — when a generated variant like `WpAgent`
/// has 0 matches, this function finds `WPAgent` (the actual casing in the codebase).
pub fn find_case_insensitive_matches(text: &str, term: &str) -> Vec<(usize, String)> {
    let text_lower = text.to_lowercase();
    let term_lower = term.to_lowercase();
    let term_len = term.len();
    let mut matches = Vec::new();

    if term_len == 0 || term_len > text.len() {
        return matches;
    }

    let mut start = 0;
    while let Some(pos) = text_lower[start..].find(&term_lower) {
        let abs = start + pos;
        let actual = &text[abs..abs + term_len];
        matches.push((abs, actual.to_string()));
        start = abs + 1;
    }

    matches
}
