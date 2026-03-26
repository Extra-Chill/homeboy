//! items — extracted from decompose.rs.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use crate::extension::{self, ParsedItem};
use serde::{Deserialize, Serialize};
use crate::Result;
use super::super::move_items::{MoveOptions, MoveResult};
use super::sanitize_module_name;
use super::merge_small_groups_protected;
use super::find_section_for_item;
use super::is_non_extractable_item;
use super::DecomposeGroup;
use super::rebalance_for_viable_parent_surface;
use super::to_snake_case;
use super::colocate_types;
use super::cluster_by_name_segments;
use super::extract_sections;


pub(crate) fn group_items(file: &str, items: &[ParsedItem], content: &str) -> Vec<DecomposeGroup> {
    let source = PathBuf::from(file);
    let raw_stem = source
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("module")
        .to_string();
    let base_dir = source
        .parent()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();

    // When the source file is mod.rs, submodules go in the same directory
    // (they're siblings of mod.rs, not children of a "mod/" subdirectory).
    // e.g., src/core/code_audit/mod.rs → src/core/code_audit/types.rs
    //   NOT src/core/code_audit/mod/types.rs
    let (stem, effective_base) = if raw_stem == "mod" {
        // mod.rs: use parent dir as the target directory, no extra nesting
        (String::new(), base_dir.clone())
    } else {
        (raw_stem, base_dir.clone())
    };

    // Phase 1: Separate by kind
    let mut type_items: Vec<&ParsedItem> = Vec::new();
    let mut const_items: Vec<&ParsedItem> = Vec::new();
    let mut fn_items: Vec<&ParsedItem> = Vec::new();

    for item in items {
        match item.kind.as_str() {
            "struct" | "enum" | "trait" | "type_alias" => type_items.push(item),
            "impl" => type_items.push(item),
            "const" | "static" => const_items.push(item),
            "function" => fn_items.push(item),
            _ => fn_items.push(item),
        }
    }

    let parent_kept_functions = identify_parent_kept_functions(file, &fn_items, content);

    // Phase 2: Try section headers first (strongest signal)
    let sections = extract_sections(content);
    let mut fn_buckets: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut section_assigned: HashSet<String> = HashSet::new();

    if sections.len() >= 2 {
        // Assign functions to sections based on line numbers
        for item in &fn_items {
            if let Some(section) = find_section_for_item(&sections, item.start_line) {
                fn_buckets
                    .entry(section.to_string())
                    .or_default()
                    .push(item.name.clone());
                section_assigned.insert(item.name.clone());
            }
        }
    }

    // Phase 3: For functions not in sections, use call graph + name clustering
    let unassigned_fns: Vec<&ParsedItem> = fn_items
        .iter()
        .copied()
        .filter(|i| !section_assigned.contains(&i.name))
        .filter(|i| !parent_kept_functions.contains(&i.name))
        .collect();

    if !unassigned_fns.is_empty() {
        let fn_name_set: HashSet<&str> = unassigned_fns.iter().map(|i| i.name.as_str()).collect();
        let call_graph = build_call_graph(&unassigned_fns, &fn_name_set);
        let (components, hub_names) = call_graph_components(&call_graph);
        let mut graph_assigned: HashSet<String> = HashSet::new();

        // Hub functions (orchestrators) are excluded from clusters.
        // They stay unassigned and will be handled by name clustering below,
        // or stay in the parent module if they don't cluster with anything.
        for hub in &hub_names {
            graph_assigned.insert(hub.clone());
        }

        for (_, members) in &components {
            let label = pick_cluster_label(members, &call_graph);
            for member in members {
                graph_assigned.insert(member.clone());
            }
            fn_buckets
                .entry(label)
                .or_default()
                .extend(members.iter().cloned());
        }

        // Remaining (including hubs): name-based clustering
        let still_unassigned: Vec<&str> = unassigned_fns
            .iter()
            .map(|i| i.name.as_str())
            .filter(|n| !graph_assigned.contains(*n) || hub_names.contains(&n.to_string()))
            .collect();

        if !still_unassigned.is_empty() {
            let clusters = cluster_by_name_segments(&still_unassigned);
            for (cluster_name, names) in clusters {
                for name in names {
                    fn_buckets
                        .entry(cluster_name.clone())
                        .or_default()
                        .push(name.to_string());
                }
            }
        }
    }

    // Phase 4: Co-locate types — group impls with their struct/enum/trait
    let mut type_buckets: BTreeMap<String, Vec<String>> = BTreeMap::new();
    if !type_items.is_empty() {
        let type_clusters = colocate_types(&type_items);
        for (cluster_name, names) in type_clusters {
            type_buckets.entry(cluster_name).or_default().extend(names);
        }
    }

    // Constants
    if !const_items.is_empty() {
        for item in &const_items {
            type_buckets
                .entry("constants".to_string())
                .or_default()
                .push(item.name.clone());
        }
    }

    // Consolidate small type groups into "types"
    let mut consolidated_type_buckets: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut small_type_overflow: Vec<String> = Vec::new();

    for (name, names) in type_buckets {
        if names.len() >= MIN_CLUSTER_SIZE {
            consolidated_type_buckets.insert(name, names);
        } else {
            small_type_overflow.extend(names);
        }
    }
    if !small_type_overflow.is_empty() {
        consolidated_type_buckets
            .entry("types".to_string())
            .or_default()
            .extend(small_type_overflow);
    }

    // Merge type and function buckets, protecting type groups from merging
    // into function groups by tracking which bucket names are type-origin
    let mut type_bucket_keys: HashSet<String> = HashSet::new();
    let mut buckets: BTreeMap<String, Vec<String>> = fn_buckets;
    for (name, names) in consolidated_type_buckets {
        let key = if buckets.contains_key(&name) {
            format!("types_{}", name)
        } else {
            name
        };
        type_bucket_keys.insert(key.clone());
        buckets.entry(key).or_default().extend(names);
    }

    // Deduplicate within buckets — but skip type buckets because impl names
    // match their target struct/enum names (e.g., struct Foo + impl Foo both
    // have name "Foo" and should both be kept)
    for (bucket_key, names) in buckets.iter_mut() {
        if type_bucket_keys.contains(bucket_key) {
            continue; // Type buckets may have struct name == impl name, keep both
        }
        let mut seen = HashSet::new();
        names.retain(|name| seen.insert(name.clone()));
    }

    // Phase 5: Merge tiny function groups into nearest relative
    // Type groups are protected — they merge only into other type groups or stay as-is
    let buckets = merge_small_groups_protected(buckets, &type_bucket_keys);

    // Phase 6: Split oversized function groups (don't split type groups)
    let type_group_names: HashSet<String> = type_items
        .iter()
        .map(|i| {
            if type_items.len() <= 1 {
                "types".to_string()
            } else {
                to_snake_case(&i.name)
            }
        })
        .chain(std::iter::once("types".to_string()))
        .chain(std::iter::once("trait_impls".to_string()))
        .chain(std::iter::once("constants".to_string()))
        .collect();

    let mut final_buckets: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (name, names) in buckets {
        if names.len() > MAX_GROUP_SIZE && !type_group_names.contains(&name) {
            let sub_groups = split_oversized_group(&name, &names);
            for (sub_name, sub_names) in sub_groups {
                final_buckets.entry(sub_name).or_default().extend(sub_names);
            }
        } else {
            final_buckets.insert(name, names);
        }
    }

    let final_buckets =
        rebalance_for_viable_parent_surface(file, content, &fn_items, final_buckets);

    let ext = source.extension().and_then(|e| e.to_str()).unwrap_or("rs");

    final_buckets
        .into_iter()
        .filter(|(_, names)| !names.is_empty())
        .map(|(group, names)| {
            let safe_name = sanitize_module_name(&group);
            DecomposeGroup {
                suggested_target: if stem.is_empty() {
                    // mod.rs: submodules go in the same directory
                    if effective_base.is_empty() {
                        format!("{safe_name}.{ext}")
                    } else {
                        format!("{effective_base}/{safe_name}.{ext}")
                    }
                } else if effective_base.is_empty() {
                    format!("{stem}/{safe_name}.{ext}")
                } else {
                    format!("{effective_base}/{stem}/{safe_name}.{ext}")
                },
                name: group,
                item_names: names,
            }
        })
        .collect()
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

pub(crate) fn filter_extractable_items(
    file: &str,
    content: &str,
    items: Vec<ParsedItem>,
    warnings: &mut Vec<String>,
) -> Vec<ParsedItem> {
    let mut filtered = Vec::new();

    for item in items {
        if is_non_extractable_item(file, content, &item) {
            warnings.push(format!(
                "Skipped non-extractable item '{}' ({} lines {}-{}) during decompose planning",
                item.name, item.kind, item.start_line, item.end_line
            ));
            continue;
        }
        filtered.push(item);
    }

    filtered
}
