//! String comparison and normalization utilities.

use std::cmp::Ordering;

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
}
