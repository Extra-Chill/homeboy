//! section_header_parsing — extracted from decompose.rs.

use super::item;
use super::to_snake_case;
use super::DecomposeGroup;
use super::DecomposePlan;
use super::Foo;
use super::Section;
use crate::Result;
use std::path::{Path, PathBuf};

pub fn apply_plan_skeletons(plan: &DecomposePlan, root: &Path) -> Result<Vec<String>> {
    let mut created = Vec::new();

    for group in &plan.groups {
        let path = root.join(&group.suggested_target);
        if path.exists() {
            continue;
        }

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                crate::Error::internal_io(
                    e.to_string(),
                    Some(format!("create directory {}", parent.display())),
                )
            })?;
        }

        let header = format!(
            "// Decompose skeleton for group: {}\n// Planned items: {}\n\n",
            group.name,
            group.item_names.join(", ")
        );

        std::fs::write(&path, header).map_err(|e| {
            crate::Error::internal_io(e.to_string(), Some(format!("write {}", path.display())))
        })?;
        created.push(group.suggested_target.clone());
    }

    Ok(created)
}

/// Extract section headers from file content.
///
/// Recognizes patterns like:
/// - `// === Section Name ===`
/// - `// --- Section Name ---`
/// - `// *** Section Name ***`
/// - `// Section Name` (preceded by a blank line + separator comment)
pub(crate) fn extract_sections(content: &str) -> Vec<Section> {
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
                    start_line: i + 1, // 1-indexed
                });
            }
        } else if separator_re.is_match(line) {
            // Check if the next non-empty line is a comment with a section name
            // (handles the pattern: // ===\n// Section Name\n// ===)
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
/// Hyphens are converted to underscores because Rust module names must be
/// valid identifiers (no hyphens). "Whole-file move" → "whole_file_move".
pub(crate) fn section_name_to_slug(name: &str) -> String {
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

/// Ensure a group name is a valid Rust module name (identifier).
///
/// Rust identifiers allow `[a-zA-Z_][a-zA-Z0-9_]*`. This is a safety net
/// applied at the final filename construction point — even if earlier stages
/// produce names with invalid characters (hyphens, dots, etc.), the filename
/// will be valid.
pub(crate) fn sanitize_module_name(name: &str) -> String {
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

    // Collapse consecutive underscores and trim leading/trailing
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

/// Truncate a module name to at most `MAX_MODULE_NAME_WORDS` meaningful words.
///
/// Stop words (prepositions, articles) are dropped entirely rather than counted
/// toward the limit. This produces names like `grammar_loading` instead of
/// `grammar_definition_loaded_from_extension_toml_json`.
pub(crate) fn truncate_module_name(name: &str) -> String {
    let parts: Vec<&str> = name.split('_').filter(|s| !s.is_empty()).collect();

    let mut meaningful_count = 0;
    let mut kept: Vec<&str> = Vec::new();

    for part in &parts {
        if is_stop_word(part) {
            // Drop stop words entirely — they add length without meaning
            continue;
        }
        meaningful_count += 1;
        kept.push(part);
        if meaningful_count >= MAX_MODULE_NAME_WORDS {
            break;
        }
    }

    if kept.is_empty() {
        // All words were stop words; fall back to the first segment
        parts.first().map(|s| s.to_string()).unwrap_or_default()
    } else {
        kept.join("_")
    }
}

/// Assign an item to a section based on its line number.
pub(crate) fn find_section_for_item(sections: &[Section], item_start_line: usize) -> Option<&str> {
    // Find the last section whose start_line is <= item_start_line
    sections
        .iter()
        .rev()
        .find(|s| s.start_line <= item_start_line)
        .map(|s| s.name.as_str())
}

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
