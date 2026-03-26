//! merge_small — extracted from decompose.rs.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use serde::{Deserialize, Serialize};
use crate::extension::{self, ParsedItem};
use crate::Result;
use super::super::move_items::{MoveOptions, MoveResult};


/// Merge groups with fewer than MERGE_THRESHOLD items into the nearest relative.
///
/// Type groups (tracked by `type_keys`) are protected: they only merge into
/// other type groups, never into function groups. This prevents types from
/// leaking into function clusters when deduplication shrinks a type group.
pub(crate) fn merge_small_groups_protected(
    mut buckets: BTreeMap<String, Vec<String>>,
    type_keys: &HashSet<String>,
) -> BTreeMap<String, Vec<String>> {
    let small_keys: Vec<String> = buckets
        .iter()
        .filter(|(_, names)| names.len() < MERGE_THRESHOLD)
        .map(|(k, _)| k.clone())
        .collect();

    if small_keys.is_empty() || buckets.len() <= 1 {
        return buckets;
    }

    for key in small_keys {
        let names = match buckets.remove(&key) {
            Some(n) => n,
            None => continue,
        };

        let is_type_group = type_keys.contains(&key);

        // Find the best merge target, respecting type/function boundary
        let best_target = buckets
            .keys()
            .filter(|k| {
                if is_type_group {
                    // Type groups can only merge into other type groups
                    type_keys.contains(*k)
                } else {
                    // Function groups can merge into any non-type group
                    !type_keys.contains(*k)
                }
            })
            .max_by_key(|k| {
                let similarity = key.split('_').filter(|seg| k.contains(seg)).count();
                let size = buckets.get(*k).map(|v| v.len()).unwrap_or(0);
                (similarity, size)
            })
            .cloned();

        if let Some(target) = best_target {
            buckets.entry(target).or_default().extend(names);
        } else {
            // No compatible target — keep as-is
            buckets.insert(key, names);
        }
    }

    buckets
}

/// Merge groups with fewer than MERGE_THRESHOLD items into the nearest relative.
#[cfg(test)]
pub(crate) fn merge_small_groups(buckets: BTreeMap<String, Vec<String>>) -> BTreeMap<String, Vec<String>> {
    merge_small_groups_protected(buckets, &HashSet::new())
}
