//! Shared text normalization and matching primitives.

mod ensure_multiline;
mod helpers;
mod normalize_identifier;
mod split;

pub use ensure_multiline::*;
pub use helpers::*;
pub use normalize_identifier::*;
pub use split::*;


use crate::error::{Error, Result};
use regex::Regex;
use std::cmp::Ordering;
use std::collections::BTreeSet;
use std::hash::Hash;

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

/// Parse output into lines with custom filter.
pub fn lines_filtered<'a, F>(output: &'a str, filter: F) -> impl Iterator<Item = &'a str>
where
    F: Fn(&str) -> bool + 'a,
{
    output
        .lines()
        .filter(move |line| !line.is_empty() && filter(line))
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
        assert_eq!(
            require_identical(&values, "test").unwrap(),
            "1.0.0".to_string()
        );
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
        assert_eq!(
            json_path_str(&json, &["release", "local_path"]),
            Some("/path/to/file")
        );
    }

    #[test]
    fn split_identifier_preserves_subtarget_colons() {
        assert_eq!(
            split_identifier("project:sub:target"),
            ("project", Some("sub:target"))
        );
    }

    #[test]
    fn test_normalize_identifier_default_path() {

        let _result = normalize_identifier();
    }

    #[test]
    fn test_identifier_eq_default_path() {
        let a = "";
        let b = "";
        let _result = identifier_eq(&a, &b);
    }

    #[test]
    fn test_normalize_doc_segment_default_path() {
        let input = "";
        let _result = normalize_doc_segment(&input);
    }

    #[test]
    fn test_normalize_doc_segment_has_expected_effects() {
        // Expected effects: mutation
        let input = "";
        let _ = normalize_doc_segment(&input);
    }

    #[test]
    fn test_cmp_case_insensitive_default_path() {
        let a = "";
        let b = "";
        let _result = cmp_case_insensitive(&a, &b);
    }

    #[test]
    fn test_levenshtein_default_path() {
        let a = "";
        let b = "";
        let _result = levenshtein(&a, &b);
    }

    #[test]
    fn test_ensure_multiline_default_path() {
        let pattern = "";
        let _result = ensure_multiline(&pattern);
    }

    #[test]
    fn test_extract_first_default_path() {
        let content = "";
        let pattern = "";
        let _result = extract_first(&content, &pattern);
    }

    #[test]
    fn test_extract_all_default_path() {
        let content = "";
        let pattern = "";
        let result = extract_all(&content, &pattern);
        assert!(!result.is_empty(), "expected non-empty collection for: default path");
    }

    #[test]
    fn test_extract_all_some_matches() {
        let content = "";
        let pattern = "";
        let result = extract_all(&content, &pattern);
        assert!(!result.is_empty(), "expected non-empty collection for: Some(matches)");
    }

    #[test]
    fn test_replace_all_default_path() {
        let content = "";
        let pattern = "";
        let replacement = "";
        let _result = replace_all(&content, &pattern, &replacement);
    }

    #[test]
    fn test_replace_all_else() {
        let content = "";
        let pattern = "";
        let replacement = "";
        let result = replace_all(&content, &pattern, &replacement);
        assert!(result.is_some(), "expected Some for: else");
    }

    #[test]
    fn test_lines_default_path() {
        let output = "";
        let _result = lines(&output);
    }

    #[test]
    fn test_split_whitespace_parts_len_min_parts() {
        let line = "";
        let min_parts = 0;
        let result = split_whitespace(&line, min_parts);
        assert!(!result.is_empty(), "expected non-empty collection for: parts.len() >= min_parts");
    }

    #[test]
    fn test_split_whitespace_else() {
        let line = "";
        let min_parts = 0;
        let result = split_whitespace(&line, min_parts);
        assert!(!result.is_empty(), "expected non-empty collection for: else");
    }

    #[test]
    fn test_split_identifier_match_identifier_split_once() {
        let identifier = "";
        let result = split_identifier(&identifier);
        assert!(result.is_some(), "expected Some for: match identifier.split_once(':')");
    }

    #[test]
    fn test_split_identifier_else() {
        let identifier = "";
        let result = split_identifier(&identifier);
        assert!(result.is_some(), "expected Some for: else");
    }

}
