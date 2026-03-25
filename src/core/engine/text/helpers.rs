//! helpers — extracted from text.rs.

use crate::error::{Error, Result};
use std::cmp::Ordering;
use std::collections::BTreeSet;
use std::hash::Hash;
use regex::Regex;


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
