//! name_segment_clustering — extracted from decompose.rs.

use super::super::*;


/// Split a function name into semantic segments by `_`.
pub(crate) fn name_segments(name: &str) -> Vec<String> {
    name.split('_')
        .filter(|s| !s.is_empty() && s.len() > 1) // skip single-char segments
        .map(|s| s.to_lowercase())
        .collect()
}

/// Generate multi-word prefixes from a name (e.g., "extract_changes_from_diff" → ["extract_changes", "extract"]).
pub(crate) fn name_prefixes(name: &str) -> Vec<String> {
    let parts: Vec<&str> = name.split('_').filter(|s| !s.is_empty()).collect();
    let mut prefixes = Vec::new();

    // 2-word prefix (most specific)
    if parts.len() >= 2 {
        prefixes.push(format!("{}_{}", parts[0], parts[1]).to_lowercase());
    }
    // 1-word prefix
    if !parts.is_empty() && parts[0].len() > 1 {
        prefixes.push(parts[0].to_lowercase());
    }

    prefixes
}

/// Words that are too generic to be useful as cluster names.
pub(crate) fn is_stop_word(word: &str) -> bool {
    matches!(
        word,
        "get"
            | "set"
            | "new"
            | "is"
            | "has"
            | "the"
            | "for"
            | "from"
            | "into"
            | "with"
            | "to"
            | "in"
            | "of"
            | "fn"
            | "pub"
            | "run"
            | "do"
            | "make"
            | "on"
            | "by"
            | "or"
            | "an"
            | "at"
            | "no"
            | "not"
            | "can"
            | "all"
    )
}
