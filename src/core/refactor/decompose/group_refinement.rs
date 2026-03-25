//! group_refinement — extracted from decompose.rs.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use crate::core::scaffold::load_extension_grammar;
use crate::extension::grammar_items;
use crate::extension::{self, ParsedItem};
use crate::Result;
use serde::{Deserialize, Serialize};
use super::super::move_items::{MoveOptions, MoveResult};
use super::truncate_module_name;
use super::cluster_by_name_segments;
use super::parse_items;
use super::DecomposePlan;


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

/// Split an oversized group into sub-groups using name clustering.
pub(crate) fn split_oversized_group(name: &str, names: &[String]) -> Vec<(String, Vec<String>)> {
    let name_refs: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
    let sub_clusters = cluster_by_name_segments(&name_refs);

    if sub_clusters.len() <= 1 {
        return vec![(name.to_string(), names.to_vec())];
    }

    sub_clusters
        .into_iter()
        .map(|(sub_name, sub_names)| {
            // Use the sub-cluster name directly instead of compounding
            // parent_child names. This prevents verbose names like
            // `convenience_helpers_for_feature_consumers` from stacking.
            let label = if sub_name == "helpers" {
                name.to_string()
            } else {
                truncate_module_name(&sub_name)
            };
            (
                label,
                sub_names.into_iter().map(|s| s.to_string()).collect(),
            )
        })
        .collect()
}

/// Validate that parsed items have balanced braces before writing.
///
/// This prevents the kind of corruption that killed upgrade.rs — if the parser
/// produced items with unbalanced braces, we abort before writing anything.
pub(crate) fn validate_plan_sources(plan: &DecomposePlan, root: &Path) -> Result<()> {
    let source_path = root.join(&plan.file);
    let content = std::fs::read_to_string(&source_path).map_err(|e| {
        crate::Error::internal_io(e.to_string(), Some("pre-write validation".to_string()))
    })?;

    let ext = Path::new(&plan.file).extension().and_then(|e| e.to_str());
    let grammar = ext.and_then(|ext| {
        let manifest = extension::find_extension_for_file_ext(ext, "refactor")?;
        let ext_path = manifest.extension_path.as_deref()?;
        load_extension_grammar(Path::new(ext_path), ext)
    });

    if let Some(grammar) = grammar {
        // Re-parse and validate each item's source has balanced braces
        let items = grammar_items::parse_items(&content, &grammar);
        for item in &items {
            if !grammar_items::validate_brace_balance(&item.source, &grammar) {
                return Err(crate::Error::validation_invalid_argument(
                    "file",
                    format!(
                        "Pre-write validation failed: item '{}' (lines {}-{}) has unbalanced braces. \
                         Aborting to prevent file corruption.",
                        item.name, item.start_line, item.end_line
                    ),
                    None,
                    Some(vec![
                        "This usually means the parser misjudged item boundaries".to_string(),
                        "Try running without --write to inspect the plan first".to_string(),
                    ]),
                ));
            }
        }
    }

    Ok(())
}

pub(crate) fn dedupe_parsed_items(items: Vec<ParsedItem>) -> Vec<ParsedItem> {
    let mut seen = HashSet::new();
    let mut deduped = Vec::new();

    for item in items {
        let key = (
            item.kind.clone(),
            item.name.clone(),
            item.start_line,
            item.end_line,
        );

        if seen.insert(key) {
            deduped.push(item);
        }
    }

    deduped
}
