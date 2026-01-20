//! Core parsing primitives for text extraction and validation.
//!
//! This module provides the foundational layer for extracting structured data
//! from text content. All parsing operations in homeboy (versions, changelogs,
//! command output, git tags) are built on these primitives.

use crate::error::{Error, Result};
use regex::Regex;
use std::collections::BTreeSet;
use std::hash::Hash;
use std::path::PathBuf;

/// Extract first match from content using regex pattern with capture group.
/// Pattern must contain exactly one capture group for the value to extract.
/// Content is trimmed before matching.
pub fn extract_first(content: &str, pattern: &str) -> Option<String> {
    let re = Regex::new(pattern).ok()?;
    re.captures(content.trim())
        .and_then(|caps| caps.get(1))
        .map(|m| m.as_str().to_string())
}

/// Extract all matches from content using regex pattern with capture group.
/// Returns empty Vec if pattern is invalid, None only on regex compile error.
pub fn extract_all(content: &str, pattern: &str) -> Option<Vec<String>> {
    let re = Regex::new(pattern).ok()?;
    let matches: Vec<String> = re
        .captures_iter(content.trim())
        .filter_map(|caps| caps.get(1).map(|m| m.as_str().to_string()))
        .collect();
    Some(matches)
}

/// Replace all matches of capture group with new value.
/// Returns (new_content, replacement_count).
pub fn replace_all(content: &str, pattern: &str, replacement: &str) -> Option<(String, usize)> {
    let re = Regex::new(pattern).ok()?;
    let mut count = 0usize;

    let replaced = re
        .replace_all(content, |caps: &regex::Captures| {
            count += 1;
            let full_match = caps.get(0).map(|m| m.as_str()).unwrap_or("");
            let captured = caps.get(1).map(|m| m.as_str()).unwrap_or("");
            full_match.replacen(captured, replacement, 1)
        })
        .to_string();

    Some((replaced, count))
}

/// Validate all extracted values are identical, return the canonical value.
/// Used for version consistency checks across multiple files.
pub fn require_identical<T>(values: &[T], context: &str) -> Result<T>
where
    T: Clone + Eq + Hash + std::fmt::Display + Ord,
{
    if values.is_empty() {
        return Err(Error::internal_unexpected(format!(
            "No values found in {}",
            context
        )));
    }

    let unique: BTreeSet<&T> = values.iter().collect();
    if unique.len() != 1 {
        let items: Vec<String> = unique.iter().map(|v| v.to_string()).collect();
        return Err(Error::internal_unexpected(format!(
            "Multiple different values found in {}: {}",
            context,
            items.join(", ")
        )));
    }

    Ok(values[0].clone())
}

/// Extract all matches and validate they are identical.
/// Combines extract_all + require_identical for the common pattern.
pub fn extract_unique(content: &str, pattern: &str, context: &str) -> Result<String> {
    let values = extract_all(content, pattern).ok_or_else(|| {
        Error::validation_invalid_argument(
            "pattern",
            format!("Invalid regex pattern: {}", pattern),
            None,
            None,
        )
    })?;

    if values.is_empty() {
        return Err(Error::internal_unexpected(format!(
            "No matches found in {} using pattern: {}",
            context, pattern
        )));
    }

    require_identical(&values, context)
}

/// Parse output into non-empty lines.
pub fn lines(output: &str) -> impl Iterator<Item = &str> {
    output.lines().filter(|line| !line.is_empty())
}

/// Parse output into lines with custom filter.
pub fn lines_filtered<'a, F>(output: &'a str, filter: F) -> impl Iterator<Item = &'a str>
where
    F: Fn(&str) -> bool + 'a,
{
    output
        .lines()
        .filter(move |line| !line.is_empty() && filter(line))
}

/// Parse line by splitting on whitespace, returning parts if expected count met.
pub fn split_whitespace(line: &str, min_parts: usize) -> Option<Vec<&str>> {
    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.len() >= min_parts {
        Some(parts)
    } else {
        None
    }
}

/// Resolve path that may be absolute or relative to base.
pub fn resolve_path(base: &str, file: &str) -> PathBuf {
    if file.starts_with('/') {
        PathBuf::from(file)
    } else {
        PathBuf::from(base).join(file)
    }
}

/// Resolve path and return as String.
pub fn resolve_path_string(base: &str, file: &str) -> String {
    resolve_path(base, file).to_string_lossy().to_string()
}

/// Deduplicate preserving first occurrence order.
pub fn dedupe<T>(items: Vec<T>) -> Vec<T>
where
    T: Clone + Eq + Hash,
{
    let mut seen = std::collections::HashSet::new();
    items
        .into_iter()
        .filter(|item| seen.insert(item.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_first_finds_version() {
        let content = r#"Version: 1.2.3"#;
        let pattern = r"Version:\s*(\d+\.\d+\.\d+)";
        assert_eq!(extract_first(content, pattern), Some("1.2.3".to_string()));
    }

    #[test]
    fn extract_first_returns_none_on_no_match() {
        let content = "no version here";
        let pattern = r"Version:\s*(\d+\.\d+\.\d+)";
        assert_eq!(extract_first(content, pattern), None);
    }

    #[test]
    fn extract_all_finds_multiple() {
        let content = "v1.0.0 and v2.0.0";
        let pattern = r"v(\d+\.\d+\.\d+)";
        let result = extract_all(content, pattern).unwrap();
        assert_eq!(result, vec!["1.0.0", "2.0.0"]);
    }

    #[test]
    fn replace_all_counts_replacements() {
        let content = "v1.0.0 and v1.0.0";
        let pattern = r"v(\d+\.\d+\.\d+)";
        let (replaced, count) = replace_all(content, pattern, "2.0.0").unwrap();
        assert_eq!(replaced, "v2.0.0 and v2.0.0");
        assert_eq!(count, 2);
    }

    #[test]
    fn require_identical_passes_single_value() {
        let values = vec!["1.0.0".to_string()];
        assert_eq!(
            require_identical(&values, "test").unwrap(),
            "1.0.0".to_string()
        );
    }

    #[test]
    fn require_identical_passes_duplicates() {
        let values = vec!["1.0.0".to_string(), "1.0.0".to_string()];
        assert_eq!(
            require_identical(&values, "test").unwrap(),
            "1.0.0".to_string()
        );
    }

    #[test]
    fn require_identical_fails_on_different() {
        let values = vec!["1.0.0".to_string(), "2.0.0".to_string()];
        assert!(require_identical(&values, "test").is_err());
    }

    #[test]
    fn lines_filters_empty() {
        let output = "line1\n\nline2\n";
        let result: Vec<&str> = lines(output).collect();
        assert_eq!(result, vec!["line1", "line2"]);
    }

    #[test]
    fn resolve_path_handles_absolute() {
        let result = resolve_path_string("/base", "/absolute/path");
        assert_eq!(result, "/absolute/path");
    }

    #[test]
    fn resolve_path_handles_relative() {
        let result = resolve_path_string("/base", "relative/path");
        assert_eq!(result, "/base/relative/path");
    }

    #[test]
    fn dedupe_preserves_order() {
        let items = vec!["a", "b", "a", "c", "b"];
        let result = dedupe(items);
        assert_eq!(result, vec!["a", "b", "c"]);
    }
}
