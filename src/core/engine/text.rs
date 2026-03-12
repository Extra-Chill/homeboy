//! Shared text normalization and matching primitives.

use crate::error::{Error, Result};
use regex::Regex;
use std::cmp::Ordering;
use std::collections::BTreeSet;
use std::hash::Hash;

pub(crate) fn normalize_identifier(input: &str) -> String {
    input.trim().to_lowercase()
}

pub fn identifier_eq(a: &str, b: &str) -> bool {
    normalize_identifier(a) == normalize_identifier(b)
}

pub fn normalize_doc_segment(input: &str) -> String {
    input.trim().to_lowercase().replace([' ', '\t'], "-")
}

pub fn cmp_case_insensitive(a: &str, b: &str) -> Ordering {
    a.to_lowercase().cmp(&b.to_lowercase())
}

/// Levenshtein edit distance between two strings.
///
/// Uses space-optimized two-row algorithm (O(n) space instead of O(m*n)).
pub fn levenshtein(a: &str, b: &str) -> usize {
    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();
    let a_len = a_chars.len();
    let b_len = b_chars.len();

    if a_len == 0 {
        return b_len;
    }
    if b_len == 0 {
        return a_len;
    }

    let mut prev_row: Vec<usize> = (0..=b_len).collect();
    let mut curr_row: Vec<usize> = vec![0; b_len + 1];

    for (i, a_char) in a_chars.iter().enumerate() {
        curr_row[0] = i + 1;
        for (j, b_char) in b_chars.iter().enumerate() {
            let cost = if a_char == b_char { 0 } else { 1 };
            curr_row[j + 1] = (prev_row[j + 1] + 1)
                .min(curr_row[j] + 1)
                .min(prev_row[j] + cost);
        }
        std::mem::swap(&mut prev_row, &mut curr_row);
    }

    prev_row[b_len]
}

/// Ensure a regex pattern has multiline mode enabled.
pub fn ensure_multiline(pattern: &str) -> String {
    if pattern.contains("(?m)") {
        pattern.to_string()
    } else {
        format!("(?m){}", pattern)
    }
}

/// Extract first match from content using regex pattern with capture group.
pub fn extract_first(content: &str, pattern: &str) -> Option<String> {
    let re = Regex::new(&ensure_multiline(pattern)).ok()?;
    re.captures(content.trim())
        .and_then(|caps| caps.get(1))
        .map(|m| m.as_str().to_string())
}

/// Extract all matches from content using regex pattern with capture group.
pub fn extract_all(content: &str, pattern: &str) -> Option<Vec<String>> {
    let re = Regex::new(&ensure_multiline(pattern)).ok()?;
    let matches: Vec<String> = re
        .captures_iter(content.trim())
        .filter_map(|caps| caps.get(1).map(|m| m.as_str().to_string()))
        .collect();
    Some(matches)
}

/// Replace all matches of capture group with new value.
pub fn replace_all(content: &str, pattern: &str, replacement: &str) -> Option<(String, usize)> {
    let re = Regex::new(&ensure_multiline(pattern)).ok()?;
    let mut count = 0usize;
    let had_trailing_newline = content.ends_with('\n');
    let trimmed = content.trim();

    let replaced = re
        .replace_all(trimmed, |caps: &regex::Captures| {
            count += 1;
            let full_match = caps.get(0).map(|m| m.as_str()).unwrap_or("");
            let captured = caps.get(1).map(|m| m.as_str()).unwrap_or("");
            full_match.replacen(captured, replacement, 1)
        })
        .to_string();

    let result = if had_trailing_newline && !replaced.ends_with('\n') {
        format!("{}\n", replaced)
    } else {
        replaced
    };

    Some((result, count))
}

/// Validate all extracted values are identical, return the canonical value.
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

/// Parse a potentially combined project:subtarget identifier.
pub fn split_identifier(identifier: &str) -> (&str, Option<&str>) {
    match identifier.split_once(':') {
        Some((project, subtarget)) => {
            let project = project.trim();
            let subtarget = subtarget.trim();
            if subtarget.is_empty() {
                (project, None)
            } else {
                (project, Some(subtarget))
            }
        }
        None => (identifier.trim(), None),
    }
}

/// Extract a string value from a nested JSON path.
pub fn json_path_str<'a>(json: &'a serde_json::Value, path: &[&str]) -> Option<&'a str> {
    let mut current = json;
    for part in path {
        current = current.get(part)?;
    }
    current.as_str()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_identifier_trims_and_lowercases() {
        assert_eq!(normalize_identifier("  HeLLo  "), "hello");
    }

    #[test]
    fn identifier_eq_is_case_insensitive_and_trims() {
        assert!(identifier_eq("  My-Site ", "my-site"));
    }

    #[test]
    fn normalize_doc_segment_replaces_spaces_and_tabs_with_dashes() {
        assert_eq!(normalize_doc_segment("  My Topic\tName "), "my-topic-name");
    }

    #[test]
    fn cmp_case_insensitive_sorts_without_caring_about_case() {
        let mut values = vec!["b", "A", "c"];
        values.sort_by(|a, b| cmp_case_insensitive(a, b));
        assert_eq!(values, vec!["A", "b", "c"]);
    }

    #[test]
    fn levenshtein_returns_zero_for_identical_strings() {
        assert_eq!(levenshtein("hello", "hello"), 0);
    }

    #[test]
    fn levenshtein_returns_length_for_empty_other() {
        assert_eq!(levenshtein("hello", ""), 5);
        assert_eq!(levenshtein("", "world"), 5);
    }

    #[test]
    fn levenshtein_counts_substitutions() {
        assert_eq!(levenshtein("cat", "bat"), 1);
        assert_eq!(levenshtein("kitten", "sitting"), 3);
    }

    #[test]
    fn extract_first_finds_version() {
        let content = r#"Version: 1.2.3"#;
        let pattern = r"Version:\s*(\d+\.\d+\.\d+)";
        assert_eq!(extract_first(content, pattern), Some("1.2.3".to_string()));
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
    fn require_identical_passes_duplicates() {
        let values = vec!["1.0.0".to_string(), "1.0.0".to_string()];
        assert_eq!(require_identical(&values, "test").unwrap(), "1.0.0".to_string());
    }

    #[test]
    fn lines_filters_empty() {
        let output = "line1\n\nline2\n";
        let result: Vec<&str> = lines(output).collect();
        assert_eq!(result, vec!["line1", "line2"]);
    }

    #[test]
    fn dedupe_preserves_order() {
        let items = vec!["a", "b", "a", "c", "b"];
        let result = dedupe(items);
        assert_eq!(result, vec!["a", "b", "c"]);
    }

    #[test]
    fn json_path_str_extracts_nested_value() {
        let json = serde_json::json!({"release": {"local_path": "/path/to/file"}});
        assert_eq!(json_path_str(&json, &["release", "local_path"]), Some("/path/to/file"));
    }

    #[test]
    fn split_identifier_preserves_subtarget_colons() {
        assert_eq!(split_identifier("project:sub:target"), ("project", Some("sub:target")));
    }
}
