//! helpers — extracted from text.rs.

use std::cmp::Ordering;
use crate::error::{Error, Result};
use regex::Regex;
use std::collections::BTreeSet;
use std::hash::Hash;


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

/// Parse output into non-empty lines.
pub fn lines(output: &str) -> impl Iterator<Item = &str> {
    output.lines().filter(|line| !line.is_empty())
}
