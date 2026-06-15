use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use crate::core::extension::ParsedItem;

use super::DecomposeGroup;

/// Maximum items per group before we attempt to split further.
const MAX_GROUP_SIZE: usize = 15;

/// Groups below this size get merged into the nearest related group.
const MERGE_THRESHOLD: usize = 2;

/// Minimum number of items sharing a word to form a name-based cluster.
const MIN_CLUSTER_SIZE: usize = 2;

/// A section header found in source comments (e.g., `// === Models ===`).
#[derive(Debug)]
pub(super) struct Section {
    pub(super) name: String,
    start_line: usize,
}

/// Extract section headers from file content.
///
/// Recognizes patterns like:
/// - `// === Section Name ===`
/// - `// --- Section Name ---`
/// - `// *** Section Name ***`
/// - `// Section Name` (preceded by a blank line + separator comment)
pub(super) fn extract_sections(content: &str) -> Vec<Section> {
    let mut sections = Vec::new();
    let separator_re =
        regex::Regex::new(r"^\s*//\s*[=\-*]{3,}\s*$").expect("valid separator regex");
    let header_re =
        regex::Regex::new(r"^\s*//\s*[=\-*]{2,}\s+(.+?)\s+[=\-*]{2,}\s*$").expect("valid regex");

    let lines: Vec<&str> = content.lines().collect();
    for (i, line) in lines.iter().enumerate() {
        if let Some(cap) = header_re.captures(line) {
            let name = cap[1].trim().to_string();
            let slug = section_name_to_slug(&name);
            if !slug.is_empty() {
                sections.push(Section {
                    name: slug,
                    start_line: i + 1,
                });
            }
        } else if separator_re.is_match(line) {
            if let Some(next) = lines.get(i + 1) {
                let trimmed = next.trim();
                if let Some(name) = trimmed
                    .strip_prefix("//")
                    .map(|s| s.trim())
                    .filter(|s| !s.is_empty() && !s.chars().all(|c| "=-*".contains(c)))
                {
                    let slug = section_name_to_slug(name);
                    if !slug.is_empty() && !sections.iter().any(|s| s.name == slug) {
                        sections.push(Section {
                            name: slug,
                            start_line: i + 1,
                        });
                    }
                }
            }
        }
    }

    sections
}

/// Convert a section header name to a snake_case slug suitable for filenames.
///
/// Hyphens are converted to underscores so generated group filenames stay
/// identifier-like. "Whole-file move" -> "whole_file_move".
pub(super) fn section_name_to_slug(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == ' ' || c == '_' {
                c
            } else if c == '-' {
                '_'
            } else {
                ' '
            }
        })
        .collect();

    let words: Vec<String> = cleaned
        .split_whitespace()
        .map(|w| w.to_lowercase())
        .collect();

    truncate_module_name(&words.join("_"))
}

/// Ensure a group name is a valid identifier-like module name.
///
/// This is a safety net applied at the final filename construction point.
pub(super) fn sanitize_module_name(name: &str) -> String {
    let sanitized: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();

    let mut result = String::new();
    let mut prev_underscore = false;
    for c in sanitized.chars() {
        if c == '_' {
            if !prev_underscore && !result.is_empty() {
                result.push('_');
            }
            prev_underscore = true;
        } else {
            prev_underscore = false;
            result.push(c);
        }
    }
    result.trim_end_matches('_').to_string()
}

/// Maximum number of meaningful words (non-stop-words) in a module name.
const MAX_MODULE_NAME_WORDS: usize = 3;

/// Truncate a module name to at most `MAX_MODULE_NAME_WORDS` meaningful words.
pub(super) fn truncate_module_name(name: &str) -> String {
    let parts: Vec<&str> = name.split('_').filter(|s| !s.is_empty()).collect();

    let mut meaningful_count = 0;
    let mut kept: Vec<&str> = Vec::new();

    for part in &parts {
        if is_stop_word(part) {
            continue;
        }
        meaningful_count += 1;
        kept.push(part);
        if meaningful_count >= MAX_MODULE_NAME_WORDS {
            break;
        }
    }

    if kept.is_empty() {
        parts.first().map(|s| s.to_string()).unwrap_or_default()
    } else {
        kept.join("_")
    }
}

/// Assign an item to a section based on its line number.
fn find_section_for_item(sections: &[Section], item_start_line: usize) -> Option<&str> {
    sections
        .iter()
        .rev()
        .find(|s| s.start_line <= item_start_line)
        .map(|s| s.name.as_str())
}

/// Build a map of which functions call which other functions in the same file.
pub(super) fn build_call_graph(
    fn_items: &[&ParsedItem],
    fn_names: &HashSet<&str>,
) -> BTreeMap<String, HashSet<String>> {
    let mut graph: BTreeMap<String, HashSet<String>> = BTreeMap::new();

    for item in fn_items {
        let mut callees = HashSet::new();
        for name in fn_names {
            if *name != item.name && item.source.contains(name) {
                let pattern = format!(r"\b{}\b", regex::escape(name));
                if let Ok(re) = regex::Regex::new(&pattern) {
                    if re.is_match(&item.source) {
                        callees.insert(name.to_string());
                    }
                }
            }
        }
        graph.insert(item.name.clone(), callees);
    }

    graph
}

/// Maximum number of callees before a function is considered a "hub".
const HUB_THRESHOLD: usize = 4;

/// Find connected components in the call graph using hub-aware union-find.
pub(super) fn call_graph_components(
    graph: &BTreeMap<String, HashSet<String>>,
) -> (Vec<(String, Vec<String>)>, Vec<String>) {
    let all_names: Vec<String> = graph.keys().cloned().collect();
    let hubs: HashSet<&str> = graph
        .iter()
        .filter(|(_, callees)| callees.len() >= HUB_THRESHOLD)
        .map(|(name, _)| name.as_str())
        .collect();

    let non_hub_names: Vec<&String> = all_names
        .iter()
        .filter(|n| !hubs.contains(n.as_str()))
        .collect();
    let mut parent: BTreeMap<String, String> = BTreeMap::new();
    for name in &non_hub_names {
        parent.insert((*name).clone(), (*name).clone());
    }

    fn find(parent: &mut BTreeMap<String, String>, x: &str) -> String {
        let p = parent.get(x).cloned().unwrap_or_else(|| x.to_string());
        if p != x {
            let root = find(parent, &p);
            parent.insert(x.to_string(), root.clone());
            root
        } else {
            p
        }
    }

    fn union(parent: &mut BTreeMap<String, String>, a: &str, b: &str) {
        let ra = find(parent, a);
        let rb = find(parent, b);
        if ra != rb {
            parent.insert(rb, ra);
        }
    }

    for (caller, callees) in graph {
        if hubs.contains(caller.as_str()) {
            continue;
        }
        for callee in callees {
            if parent.contains_key(callee) {
                union(&mut parent, caller, callee);
            }
        }
    }

    let mut groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for name in &non_hub_names {
        let root = find(&mut parent, name);
        groups.entry(root).or_default().push((*name).clone());
    }

    let clustered: Vec<(String, Vec<String>)> = groups
        .into_iter()
        .filter(|(_, members)| members.len() >= 2)
        .collect();

    let hub_names: Vec<String> = hubs.iter().map(|s| s.to_string()).collect();
    (clustered, hub_names)
}

/// Pick a representative name for a call-graph cluster.
fn pick_cluster_label(members: &[String], graph: &BTreeMap<String, HashSet<String>>) -> String {
    if let Some(prefix) = find_dominant_prefix(members) {
        return prefix;
    }

    let mut call_count: BTreeMap<&str, usize> = BTreeMap::new();
    for member in members {
        for callee in graph.get(member).into_iter().flatten() {
            if members.iter().any(|m| m == callee) {
                *call_count.entry(callee.as_str()).or_default() += 1;
            }
        }
    }

    let raw = call_count
        .into_iter()
        .max_by_key(|(_, count)| *count)
        .map(|(name, _)| name.to_string())
        .unwrap_or_else(|| {
            members
                .first()
                .cloned()
                .unwrap_or_else(|| "group".to_string())
        });

    truncate_module_name(&raw)
}

/// Find a dominant prefix shared by most members of a cluster.
pub(super) fn find_dominant_prefix(members: &[String]) -> Option<String> {
    if members.len() < 2 {
        return None;
    }

    let threshold = (members.len() as f64 * 0.6).ceil() as usize;
    let mut prefix_counts: BTreeMap<String, usize> = BTreeMap::new();

    for member in members {
        let parts: Vec<&str> = member.split('_').filter(|s| !s.is_empty()).collect();
        if parts.len() >= 2 {
            let two_word = format!("{}_{}", parts[0], parts[1]).to_lowercase();
            if !is_stop_word(parts[0]) {
                *prefix_counts.entry(two_word).or_default() += 1;
            }
        }
        if !parts.is_empty() && !is_stop_word(parts[0]) && parts[0].len() > 2 {
            *prefix_counts.entry(parts[0].to_lowercase()).or_default() += 1;
        }
    }

    let mut candidates: Vec<_> = prefix_counts
        .into_iter()
        .filter(|(_, count)| *count >= threshold)
        .collect();

    candidates.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then_with(|| b.1.cmp(&a.1)));
    candidates.into_iter().next().map(|(prefix, _)| prefix)
}

/// Split a function name into semantic segments by `_`.
fn name_segments(name: &str) -> Vec<String> {
    name.split('_')
        .filter(|s| !s.is_empty() && s.len() > 1)
        .map(|s| s.to_lowercase())
        .collect()
}

/// Generate multi-word prefixes from a name.
pub(super) fn name_prefixes(name: &str) -> Vec<String> {
    let parts: Vec<&str> = name.split('_').filter(|s| !s.is_empty()).collect();
    let mut prefixes = Vec::new();

    if parts.len() >= 2 {
        prefixes.push(format!("{}_{}", parts[0], parts[1]).to_lowercase());
    }
    if !parts.is_empty() && parts[0].len() > 1 {
        prefixes.push(parts[0].to_lowercase());
    }

    prefixes
}

/// Cluster function names by shared name segments.
pub(super) fn cluster_by_name_segments<'a>(names: &[&'a str]) -> Vec<(String, Vec<&'a str>)> {
    if names.is_empty() {
        return Vec::new();
    }

    let mut assignments: BTreeMap<String, Vec<&'a str>> = BTreeMap::new();
    let mut assigned: HashSet<&str> = HashSet::new();

    let mut prefix_counts: BTreeMap<String, Vec<&'a str>> = BTreeMap::new();
    for name in names {
        for prefix in name_prefixes(name) {
            if !is_stop_word(prefix.split('_').next().unwrap_or("")) {
                prefix_counts.entry(prefix).or_default().push(name);
            }
        }
    }

    let mut prefix_list: Vec<_> = prefix_counts.into_iter().collect();
    prefix_list.sort_by(|a, b| {
        b.0.len()
            .cmp(&a.0.len())
            .then_with(|| b.1.len().cmp(&a.1.len()))
    });

    for (prefix, members) in &prefix_list {
        let unassigned: Vec<&'a str> = members
            .iter()
            .copied()
            .filter(|n| !assigned.contains(*n))
            .collect();
        if unassigned.len() >= MIN_CLUSTER_SIZE {
            for name in &unassigned {
                assigned.insert(name);
            }
            assignments
                .entry(prefix.clone())
                .or_default()
                .extend(unassigned);
        }
    }

    let remaining: Vec<&'a str> = names
        .iter()
        .copied()
        .filter(|n| !assigned.contains(*n))
        .collect();

    if !remaining.is_empty() {
        let mut segment_counts: BTreeMap<String, Vec<&'a str>> = BTreeMap::new();
        for name in &remaining {
            for seg in name_segments(name) {
                if !is_stop_word(&seg) {
                    segment_counts.entry(seg).or_default().push(name);
                }
            }
        }

        let mut seg_list: Vec<_> = segment_counts.into_iter().collect();
        seg_list.sort_by(|a, b| {
            b.0.len()
                .cmp(&a.0.len())
                .then_with(|| b.1.len().cmp(&a.1.len()))
        });

        for (seg, members) in &seg_list {
            let unassigned: Vec<&'a str> = members
                .iter()
                .copied()
                .filter(|n| !assigned.contains(*n))
                .collect();
            if unassigned.len() >= MIN_CLUSTER_SIZE {
                for name in &unassigned {
                    assigned.insert(name);
                }
                assignments
                    .entry(seg.clone())
                    .or_default()
                    .extend(unassigned);
            }
        }
    }

    let mut result: Vec<(String, Vec<&'a str>)> = assignments.into_iter().collect();
    let unclustered: Vec<&'a str> = names
        .iter()
        .copied()
        .filter(|n| !assigned.contains(*n))
        .collect();

    if !unclustered.is_empty() {
        result.push(("helpers".to_string(), unclustered));
    }

    result
}

/// Words that are too generic to be useful as cluster names.
pub(super) fn is_stop_word(word: &str) -> bool {
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

pub(super) fn group_items(file: &str, items: &[ParsedItem], content: &str) -> Vec<DecomposeGroup> {
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

    let (stem, effective_base) = if raw_stem == "mod" {
        (String::new(), base_dir.clone())
    } else {
        (raw_stem, base_dir.clone())
    };

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
    let sections = extract_sections(content);
    let mut fn_buckets: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut section_assigned: HashSet<String> = HashSet::new();

    if sections.len() >= 2 {
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

    let mut type_buckets: BTreeMap<String, Vec<String>> = BTreeMap::new();
    if !type_items.is_empty() {
        let type_clusters = colocate_types(&type_items);
        for (cluster_name, names) in type_clusters {
            type_buckets.entry(cluster_name).or_default().extend(names);
        }
    }

    if !const_items.is_empty() {
        for item in &const_items {
            type_buckets
                .entry("constants".to_string())
                .or_default()
                .push(item.name.clone());
        }
    }

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

    for (bucket_key, names) in buckets.iter_mut() {
        if type_bucket_keys.contains(bucket_key) {
            continue;
        }
        let mut seen = HashSet::new();
        names.retain(|name| seen.insert(name.clone()));
    }

    let buckets = merge_small_groups_protected(buckets, &type_bucket_keys);
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
            let module_name = sanitize_module_name(&group);
            DecomposeGroup {
                suggested_target: if stem.is_empty() {
                    if effective_base.is_empty() {
                        format!("{module_name}.{ext}")
                    } else {
                        format!("{effective_base}/{module_name}.{ext}")
                    }
                } else if effective_base.is_empty() {
                    format!("{stem}/{module_name}.{ext}")
                } else {
                    format!("{effective_base}/{stem}/{module_name}.{ext}")
                },
                name: group,
                item_names: names,
            }
        })
        .collect()
}

/// Group type items (struct/enum/trait + their impls) together.
pub(super) fn colocate_types(items: &[&ParsedItem]) -> Vec<(String, Vec<String>)> {
    let mut type_names: Vec<String> = Vec::new();
    let mut impl_targets: BTreeMap<String, Vec<String>> = BTreeMap::new();

    for item in items {
        match item.kind.as_str() {
            "struct" | "enum" | "trait" | "type_alias" => {
                type_names.push(item.name.clone());
            }
            "impl" => {
                let target = if let Some(pos) = item.name.find(" for ") {
                    item.name[pos + 5..].to_string()
                } else {
                    item.name.clone()
                };
                impl_targets
                    .entry(target)
                    .or_default()
                    .push(item.name.clone());
            }
            _ => {}
        }
    }

    if type_names.len() <= 1 {
        let mut names: Vec<String> = type_names;
        for impl_names in impl_targets.values() {
            names.extend(impl_names.iter().cloned());
        }
        if names.is_empty() {
            return Vec::new();
        }
        return vec![("types".to_string(), names)];
    }

    let mut groups: Vec<(String, Vec<String>)> = Vec::new();
    let mut assigned_impls: HashSet<String> = HashSet::new();

    for type_name in &type_names {
        let mut group_names = vec![type_name.clone()];
        if let Some(impls) = impl_targets.get(type_name) {
            for impl_name in impls {
                group_names.push(impl_name.clone());
                assigned_impls.insert(impl_name.clone());
            }
        }
        let group_label = to_snake_case(type_name);
        groups.push((group_label, group_names));
    }

    let orphaned: Vec<String> = impl_targets
        .values()
        .flatten()
        .filter(|name| !assigned_impls.contains(*name))
        .cloned()
        .collect();

    if !orphaned.is_empty() {
        groups.push(("trait_impls".to_string(), orphaned));
    }

    groups
}

/// Convert PascalCase to snake_case.
pub(super) fn to_snake_case(name: &str) -> String {
    let mut result = String::new();
    for (i, ch) in name.chars().enumerate() {
        if ch.is_uppercase() && i > 0 {
            result.push('_');
        }
        result.push(ch.to_lowercase().next().unwrap_or(ch));
    }
    result
}

/// Merge groups with fewer than MERGE_THRESHOLD items into the nearest relative.
fn merge_small_groups_protected(
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
        let best_target = buckets
            .keys()
            .filter(|k| {
                if is_type_group {
                    type_keys.contains(*k)
                } else {
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
            buckets.insert(key, names);
        }
    }

    buckets
}

/// Merge groups with fewer than MERGE_THRESHOLD items into the nearest relative.
#[cfg(test)]
pub(super) fn merge_small_groups(
    buckets: BTreeMap<String, Vec<String>>,
) -> BTreeMap<String, Vec<String>> {
    merge_small_groups_protected(buckets, &HashSet::new())
}

/// Split an oversized group into sub-groups using name clustering.
pub(super) fn split_oversized_group(name: &str, names: &[String]) -> Vec<(String, Vec<String>)> {
    let name_refs: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
    let sub_clusters = cluster_by_name_segments(&name_refs);

    if sub_clusters.len() <= 1 {
        return vec![(name.to_string(), names.to_vec())];
    }

    sub_clusters
        .into_iter()
        .map(|(sub_name, sub_names)| {
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

pub(super) fn is_non_extractable_item(file: &str, content: &str, item: &ParsedItem) -> bool {
    let source = item.source.trim();

    if source.contains("$Entity") || source.contains("$feature") {
        return true;
    }

    if is_effective_module_root(file, content) && is_root_orchestrator_name(&item.name) {
        return true;
    }

    if is_effective_module_root(file, content) && is_public_item_source(source) {
        return true;
    }

    if !starts_like_extractable_declaration(source, &item.kind) {
        return true;
    }

    if is_effective_module_root(file, content)
        && item.kind == "function"
        && item.source.lines().count() > 100
        && count_local_function_references(content, &item.name) == 0
    {
        return true;
    }

    false
}

pub(super) fn identify_parent_kept_functions(
    file: &str,
    fn_items: &[&ParsedItem],
    content: &str,
) -> HashSet<String> {
    let mut keep = HashSet::new();
    if !is_effective_module_root(file, content) {
        return keep;
    }

    for item in fn_items {
        if is_root_orchestrator_name(&item.name) {
            keep.insert(item.name.clone());
            continue;
        }

        if is_public_item_source(&item.source) {
            keep.insert(item.name.clone());
            continue;
        }

        let local_refs = count_local_function_references(content, &item.name);
        if local_refs >= 3 {
            keep.insert(item.name.clone());
        }
    }

    keep
}

fn count_local_function_references(content: &str, name: &str) -> usize {
    let pattern = format!(r"\b{}\s*\(", regex::escape(name));
    let Ok(re) = regex::Regex::new(&pattern) else {
        return 0;
    };
    re.find_iter(content).count().saturating_sub(1)
}

fn is_module_root(file: &str) -> bool {
    let path = Path::new(file);
    path.file_name()
        .and_then(|name| name.to_str())
        .map(|name| matches!(name, "mod.rs" | "lib.rs" | "main.rs" | "config.rs"))
        .unwrap_or(false)
}

pub(super) fn is_effective_module_root(file: &str, content: &str) -> bool {
    is_module_root(file) || has_established_module_surface(content)
}

pub(super) fn has_established_module_surface(content: &str) -> bool {
    let mut public_decl_count = 0usize;

    for line in content.lines().map(str::trim) {
        if line.starts_with("pub use ") || line.starts_with("pub mod ") || line.starts_with("mod ")
        {
            return true;
        }

        if line.starts_with("pub fn ")
            || line.starts_with("pub(crate) fn ")
            || line.starts_with("pub struct ")
            || line.starts_with("pub(crate) struct ")
            || line.starts_with("pub enum ")
            || line.starts_with("pub(crate) enum ")
            || line.starts_with("pub trait ")
            || line.starts_with("pub(crate) trait ")
            || line.starts_with("pub type ")
            || line.starts_with("pub(crate) type ")
            || line.starts_with("pub const ")
            || line.starts_with("pub(crate) const ")
        {
            public_decl_count += 1;
            if public_decl_count >= 4 {
                return true;
            }
        }
    }

    false
}

pub(super) fn is_public_item_source(source: &str) -> bool {
    let trimmed = source.trim_start();
    trimmed.starts_with("pub ") || trimmed.starts_with("pub(crate) ")
}

pub(super) fn starts_like_extractable_declaration(source: &str, kind: &str) -> bool {
    let mut saw_attribute = false;

    for line in source.lines().map(str::trim) {
        if line.is_empty() || line.starts_with("//") || line.starts_with("///") {
            continue;
        }

        if line.starts_with('#') {
            saw_attribute = true;
            continue;
        }

        let decl = matches_declaration_prefix(line, kind);
        if decl {
            return true;
        }

        if saw_attribute {
            return false;
        }

        return false;
    }

    false
}

fn matches_declaration_prefix(line: &str, kind: &str) -> bool {
    match kind {
        "function" => {
            line.starts_with("fn ")
                || line.starts_with("pub fn ")
                || line.starts_with("pub(crate) fn ")
                || line.starts_with("pub(super) fn ")
                || line.starts_with("pub async fn ")
                || line.starts_with("pub(crate) async fn ")
                || line.starts_with("async fn ")
        }
        "struct" => {
            line.starts_with("struct ")
                || line.starts_with("pub struct ")
                || line.starts_with("pub(crate) struct ")
        }
        "enum" => {
            line.starts_with("enum ")
                || line.starts_with("pub enum ")
                || line.starts_with("pub(crate) enum ")
        }
        "trait" => {
            line.starts_with("trait ")
                || line.starts_with("pub trait ")
                || line.starts_with("pub(crate) trait ")
        }
        "type_alias" => {
            line.starts_with("type ")
                || line.starts_with("pub type ")
                || line.starts_with("pub(crate) type ")
        }
        "impl" => line.starts_with("impl "),
        "const" => {
            line.starts_with("const ")
                || line.starts_with("pub const ")
                || line.starts_with("pub(crate) const ")
        }
        "static" => {
            line.starts_with("static ")
                || line.starts_with("pub static ")
                || line.starts_with("pub(crate) static ")
        }
        _ => false,
    }
}

pub(super) fn rebalance_for_viable_parent_surface(
    file: &str,
    content: &str,
    fn_items: &[&ParsedItem],
    mut buckets: BTreeMap<String, Vec<String>>,
) -> BTreeMap<String, Vec<String>> {
    if !is_effective_module_root(file, content) {
        return buckets;
    }

    let parent_surface: HashSet<String> = fn_items
        .iter()
        .filter(|item| is_root_orchestrator_name(&item.name) || is_public_item_source(&item.source))
        .map(|item| item.name.clone())
        .collect();

    if parent_surface.is_empty() {
        return buckets;
    }

    let extracted_surface_count = buckets
        .values()
        .flat_map(|names| names.iter())
        .filter(|name| parent_surface.contains(*name))
        .count();

    if extracted_surface_count >= parent_surface.len().saturating_sub(1) {
        let mut preserved = Vec::new();
        for names in buckets.values_mut() {
            let mut i = 0usize;
            while i < names.len() {
                if parent_surface.contains(&names[i]) {
                    preserved.push(names.remove(i));
                } else {
                    i += 1;
                }
            }
        }

        buckets.retain(|_, names| !names.is_empty());
        if !preserved.is_empty() {
            buckets
                .entry("parent_surface".to_string())
                .or_default()
                .extend(preserved);
        }
    }

    buckets
}

fn is_root_orchestrator_name(name: &str) -> bool {
    matches!(
        name,
        "audit_internal"
            | "audit_component"
            | "audit_path"
            | "audit_path_with_id"
            | "audit_path_scoped"
            | "build_convention_method_set"
            | "fingerprint_reference_paths"
            | "entity_crud"
    )
}
