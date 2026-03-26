//! name_segment_clustering — extracted from decompose.rs.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use serde::{Deserialize, Serialize};
use crate::extension::{self, ParsedItem};
use crate::Result;
use super::super::move_items::{MoveOptions, MoveResult};


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

/// Cluster function names by shared name segments.
///
/// Two-pass approach:
/// 1. Try multi-word prefixes first (e.g., "extract_changes" groups together)
/// 2. Fall back to single segments for remaining unclustered names
///
/// Uses MIN_CLUSTER_SIZE (2) so even pairs of related functions cluster together.
pub(crate) fn cluster_by_name_segments<'a>(names: &[&'a str]) -> Vec<(String, Vec<&'a str>)> {
    if names.is_empty() {
        return Vec::new();
    }

    let mut assignments: BTreeMap<String, Vec<&'a str>> = BTreeMap::new();
    let mut assigned: HashSet<&str> = HashSet::new();

    // Pass 1: Multi-word prefix clustering (most specific)
    let mut prefix_counts: BTreeMap<String, Vec<&'a str>> = BTreeMap::new();
    for name in names {
        for prefix in name_prefixes(name) {
            if !is_stop_word(prefix.split('_').next().unwrap_or("")) {
                prefix_counts.entry(prefix).or_default().push(name);
            }
        }
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
