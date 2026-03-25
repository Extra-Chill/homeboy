//! ensure_multiline — extracted from text.rs.

use regex::Regex;
use crate::error::{Error, Result};
use std::cmp::Ordering;
use std::collections::BTreeSet;
use std::hash::Hash;


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
