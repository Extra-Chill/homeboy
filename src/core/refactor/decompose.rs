use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::core::test_scaffold::helpers::load_extension_grammar;
use crate::extension::{self, ParsedItem};
use crate::utils::grammar_items;
use crate::Result;

use super::move_items::MoveOptions;
use super::MoveResult;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecomposePlan {
    pub file: String,
    pub strategy: String,
    pub audit_safe: bool,
    pub total_items: usize,
    pub groups: Vec<DecomposeGroup>,
    pub projected_audit_impact: DecomposeAuditImpact,
    pub checklist: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecomposeAuditImpact {
    pub estimated_new_files: usize,
    pub estimated_new_test_files: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recommended_test_files: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub likely_findings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecomposeGroup {
    pub name: String,
    pub suggested_target: String,
    pub item_names: Vec<String>,
}

pub fn build_plan(
    file: &str,
    root: &Path,
    strategy: &str,
    audit_safe: bool,
) -> Result<DecomposePlan> {
    if strategy != "grouped" {
        return Err(crate::Error::validation_invalid_argument(
            "strategy",
            format!("Unsupported strategy '{}'. Use: grouped", strategy),
            None,
            None,
        ));
    }

    let source_path = root.join(file);
    if !source_path.is_file() {
        return Err(crate::Error::validation_invalid_argument(
            "file",
            format!("Source file does not exist: {}", file),
            None,
            None,
        ));
    }

    let content = std::fs::read_to_string(&source_path)
        .map_err(|e| crate::Error::internal_io(e.to_string(), Some(format!("read {}", file))))?;

    let mut warnings = Vec::new();
    let items = parse_items(file, &content).unwrap_or_else(|| {
        warnings.push("No refactor parser available for file type; plan may be sparse".to_string());
        vec![]
    });
    let items = dedupe_parsed_items(items);

    let groups = group_items(file, &items, audit_safe);
    let projected_audit_impact = project_audit_impact(&groups, audit_safe);

    let checklist = vec![
        "Review grouping and target filenames".to_string(),
        "Review projected audit impact before applying".to_string(),
        "Apply grouped extraction in one deterministic pass (homeboy refactor decompose --write)"
            .to_string(),
        "Run cargo test and homeboy audit --changed-since origin/main".to_string(),
        if audit_safe {
            "Prefer include fragments (.inc) for low-friction audit ratchet".to_string()
        } else {
            "If creating new source modules, add matching tests for recommended test files"
                .to_string()
        },
    ];

    Ok(DecomposePlan {
        file: file.to_string(),
        strategy: strategy.to_string(),
        audit_safe,
        total_items: items.len(),
        groups,
        projected_audit_impact,
        checklist,
        warnings,
    })
}

pub fn apply_plan(plan: &DecomposePlan, root: &Path, write: bool) -> Result<Vec<MoveResult>> {
    // Pre-write validation: check brace balance on all source files involved
    if write {
        validate_plan_sources(plan, root)?;
    }

    let preview = run_moves(plan, root, false)?;
    if !write {
        return Ok(preview);
    }

    // Two-phase execution: validate first (dry-run), then apply.
    // This avoids partial writes from bad plans.
    run_moves(plan, root, true)
}

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

fn run_moves(plan: &DecomposePlan, root: &Path, write: bool) -> Result<Vec<MoveResult>> {
    let mut results = Vec::new();

    for group in &plan.groups {
        let mut seen = HashSet::new();
        let deduped_item_names: Vec<&str> = group
            .item_names
            .iter()
            .filter_map(|name| {
                if seen.insert(name.clone()) {
                    Some(name.as_str())
                } else {
                    None
                }
            })
            .collect();

        let result = super::move_items::move_items_with_options(
            &deduped_item_names,
            &plan.file,
            &group.suggested_target,
            root,
            write,
            MoveOptions {
                move_related_tests: false,
            },
        )?;
        results.push(result);
    }

    Ok(results)
}

fn project_audit_impact(groups: &[DecomposeGroup], audit_safe: bool) -> DecomposeAuditImpact {
    let mut likely_findings = Vec::new();
    let mut recommended_test_files = Vec::new();

    if audit_safe {
        likely_findings.push(
            "Lower risk mode: include fragments usually avoid new module/test convention drift"
                .to_string(),
        );
    } else {
        for group in groups {
            if let Some(test_file) = source_to_test_file(&group.suggested_target) {
                recommended_test_files.push(test_file);
            }

            if group.suggested_target.starts_with("src/commands/")
                && group.suggested_target.ends_with(".rs")
            {
                likely_findings.push(format!(
                    "{} may trigger command convention checks (run method + command tests)",
                    group.suggested_target
                ));
            }
        }

        if !recommended_test_files.is_empty() {
            likely_findings.push(
                "New src/*.rs targets likely need matching tests to avoid MissingTestFile drift"
                    .to_string(),
            );
        }
    }

    DecomposeAuditImpact {
        estimated_new_files: groups.len(),
        estimated_new_test_files: recommended_test_files.len(),
        recommended_test_files,
        likely_findings,
    }
}

fn source_to_test_file(target: &str) -> Option<String> {
    if !target.starts_with("src/") || !target.ends_with(".rs") {
        return None;
    }

    let without_src = target.strip_prefix("src/")?;
    let without_ext = without_src.strip_suffix(".rs")?;
    Some(format!("tests/{}_test.rs", without_ext))
}

fn parse_items(file: &str, content: &str) -> Option<Vec<ParsedItem>> {
    let ext = Path::new(file).extension()?.to_str()?;

    // Try core grammar engine first — faster and more robust than extension scripts
    if let Some(manifest) = extension::find_extension_for_file_ext(ext, "refactor") {
        if let Some(ext_path) = &manifest.extension_path {
            let grammar = load_extension_grammar(Path::new(ext_path), ext);
            if let Some(grammar) = grammar {
                let items = grammar_items::parse_items(content, &grammar);
                if !items.is_empty() {
                    return Some(items.into_iter().map(ParsedItem::from).collect());
                }
            }
        }

        // Fall back to extension script
        let command = serde_json::json!({
            "command": "parse_items",
            "file_path": file,
            "content": content,
        });
        let result = extension::run_refactor_script(&manifest, &command)?;
        return serde_json::from_value(result.get("items")?.clone()).ok();
    }

    None
}

/// Minimum number of items sharing a word to form a cluster.
const MIN_CLUSTER_SIZE: usize = 3;

/// Maximum items per group before we attempt to split further.
const MAX_GROUP_SIZE: usize = 15;

/// Groups below this size get merged into the nearest related group.
const MERGE_THRESHOLD: usize = 2;

fn group_items(file: &str, items: &[ParsedItem], audit_safe: bool) -> Vec<DecomposeGroup> {
    let source = PathBuf::from(file);
    let stem = source
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("module")
        .to_string();
    let base_dir = source
        .parent()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();

    // Phase 1: Separate by kind
    let mut type_items: Vec<&ParsedItem> = Vec::new();
    let mut const_items: Vec<&ParsedItem> = Vec::new();
    let mut fn_items: Vec<&ParsedItem> = Vec::new();

    for item in items {
        match item.kind.as_str() {
            "struct" | "enum" | "trait" | "type_alias" => type_items.push(item),
            "impl" => type_items.push(item), // co-located with struct in Phase 3
            "const" | "static" => const_items.push(item),
            "function" => fn_items.push(item),
            _ => fn_items.push(item), // fallback: treat unknown as functions
        }
    }

    // Phase 2: Cluster functions by shared name segments
    let mut fn_buckets: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let fn_names: Vec<&str> = fn_items.iter().map(|i| i.name.as_str()).collect();
    let fn_clusters = cluster_by_name_segments(&fn_names);

    for (cluster_name, names) in &fn_clusters {
        for name in names {
            fn_buckets
                .entry(cluster_name.clone())
                .or_default()
                .push(name.to_string());
        }
    }

    // Phase 3: Co-locate types — group impls with their struct/enum/trait
    // Types are kept in separate buckets to prevent name collision with function clusters
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

    // Consolidate small type groups: merge 1-2 item type groups into "types"
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

    // Merge type and function buckets, prefixing type group names to avoid collisions
    let mut buckets: BTreeMap<String, Vec<String>> = fn_buckets;
    for (name, names) in consolidated_type_buckets {
        let key = if buckets.contains_key(&name) {
            format!("types_{}", name)
        } else {
            name
        };
        buckets.entry(key).or_default().extend(names);
    }

    // Deduplicate within buckets
    for names in buckets.values_mut() {
        let mut seen = HashSet::new();
        names.retain(|name| seen.insert(name.clone()));
    }

    // Phase 4: Merge tiny groups into nearest relative
    let buckets = merge_small_groups(buckets);

    // Phase 5: Split oversized function groups (don't split type groups)
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

    let ext = if audit_safe { "inc" } else { "rs" };

    final_buckets
        .into_iter()
        .filter(|(_, names)| !names.is_empty())
        .map(|(group, names)| DecomposeGroup {
            suggested_target: if base_dir.is_empty() {
                format!("{}/{group}.{ext}", stem)
            } else {
                format!("{}/{}/{group}.{ext}", base_dir, stem)
            },
            name: group,
            item_names: names,
        })
        .collect()
}

/// Split a function name into semantic segments by `_`.
fn name_segments(name: &str) -> Vec<String> {
    name.split('_')
        .filter(|s| !s.is_empty() && s.len() > 1) // skip single-char segments
        .map(|s| s.to_lowercase())
        .collect()
}

/// Cluster function names by shared segments.
///
/// Finds segments that appear in >= MIN_CLUSTER_SIZE function names, then assigns
/// each function to the most specific cluster (longest shared segment). Functions
/// that don't cluster go into a catch-all group.
fn cluster_by_name_segments<'a>(names: &[&'a str]) -> Vec<(String, Vec<&'a str>)> {
    if names.is_empty() {
        return Vec::new();
    }

    // Count segment frequency across all names
    let mut segment_counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut name_segments_map: Vec<(&str, Vec<String>)> = Vec::new();

    for name in names {
        let segs = name_segments(name);
        for seg in &segs {
            *segment_counts.entry(seg.clone()).or_default() += 1;
        }
        name_segments_map.push((name, segs));
    }

    // Filter to segments appearing in enough names to form a cluster
    let cluster_segments: Vec<String> = segment_counts
        .into_iter()
        .filter(|(seg, count)| {
            *count >= MIN_CLUSTER_SIZE && !is_stop_word(seg) // skip generic words
        })
        .map(|(seg, _)| seg)
        .collect();

    // Assign each name to its best cluster (most specific shared segment)
    let mut assignments: BTreeMap<String, Vec<&str>> = BTreeMap::new();
    let mut unclustered: Vec<&str> = Vec::new();

    for (name, segs) in &name_segments_map {
        // Find the best matching cluster segment for this name
        // Prefer segments that are less common (more specific)
        let best = segs
            .iter()
            .filter(|s| cluster_segments.contains(s))
            .max_by_key(|s| s.len()); // prefer longer (more specific) segments

        if let Some(cluster_seg) = best {
            assignments
                .entry(cluster_seg.clone())
                .or_default()
                .push(name);
        } else {
            unclustered.push(name);
        }
    }

    let mut result: Vec<(String, Vec<&str>)> = assignments.into_iter().collect();

    // Put unclustered items in "helpers" if there are enough, otherwise merge
    if !unclustered.is_empty() {
        result.push(("helpers".to_string(), unclustered));
    }

    result
}

/// Words that are too generic to be useful as cluster names.
fn is_stop_word(word: &str) -> bool {
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
    )
}

/// Group type items (struct/enum/trait + their impls) together.
///
/// If there's only one type, everything goes in "types". If there are multiple,
/// each type gets its own group named after it (snake_case).
fn colocate_types(items: &[&ParsedItem]) -> Vec<(String, Vec<String>)> {
    // Collect type names (struct/enum/trait) and their impls
    let mut type_names: Vec<String> = Vec::new();
    let mut impl_targets: BTreeMap<String, Vec<String>> = BTreeMap::new();

    for item in items {
        match item.kind.as_str() {
            "struct" | "enum" | "trait" | "type_alias" => {
                type_names.push(item.name.clone());
            }
            "impl" => {
                // Extract the target type from impl name (handles "Trait for Type" format)
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

    // If only one type, just use "types"
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

    // Multiple types — group each type with its impls
    let mut groups: Vec<(String, Vec<String>)> = Vec::new();
    let mut assigned_impls: HashSet<String> = HashSet::new();

    for type_name in &type_names {
        let mut group_names = vec![type_name.clone()];

        // Find impls for this type
        if let Some(impls) = impl_targets.get(type_name) {
            for impl_name in impls {
                group_names.push(impl_name.clone());
                assigned_impls.insert(impl_name.clone());
            }
        }

        let group_label = to_snake_case(type_name);
        groups.push((group_label, group_names));
    }

    // Collect orphaned impls (impl for types not in this file)
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
fn to_snake_case(name: &str) -> String {
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
fn merge_small_groups(mut buckets: BTreeMap<String, Vec<String>>) -> BTreeMap<String, Vec<String>> {
    // Collect small groups
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

        // Find the best merge target — the group whose name shares the most
        // characters with this group's name, or the largest group as fallback
        let best_target = buckets
            .keys()
            .max_by_key(|k| {
                // Prefer name similarity, break ties by group size
                let similarity = key.split('_').filter(|seg| k.contains(seg)).count();
                let size = buckets.get(*k).map(|v| v.len()).unwrap_or(0);
                (similarity, size)
            })
            .cloned();

        if let Some(target) = best_target {
            buckets.entry(target).or_default().extend(names);
        } else {
            // No other groups exist — put back
            buckets.insert(key, names);
        }
    }

    buckets
}

/// Split an oversized group into sub-groups using name clustering.
fn split_oversized_group(name: &str, names: &[String]) -> Vec<(String, Vec<String>)> {
    let name_refs: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
    let sub_clusters = cluster_by_name_segments(&name_refs);

    // If clustering didn't help (everything ended up in one group), keep original
    if sub_clusters.len() <= 1 {
        return vec![(name.to_string(), names.to_vec())];
    }

    sub_clusters
        .into_iter()
        .map(|(sub_name, sub_names)| {
            let label = if sub_name == "helpers" {
                name.to_string() // keep parent name for the unclustered remainder
            } else {
                format!("{}_{}", name, sub_name)
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
fn validate_plan_sources(plan: &DecomposePlan, root: &Path) -> Result<()> {
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

fn dedupe_parsed_items(items: Vec<ParsedItem>) -> Vec<ParsedItem> {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn item(name: &str, kind: &str) -> ParsedItem {
        ParsedItem {
            name: name.to_string(),
            kind: kind.to_string(),
            start_line: 1,
            end_line: 10,
            source: String::new(),
            visibility: String::new(),
        }
    }

    #[test]
    fn cluster_by_name_segments_groups_shared_prefixes() {
        let names = vec![
            "extract_php_signatures",
            "extract_rust_signatures",
            "extract_js_signatures",
            "generate_stub",
            "generate_import",
            "generate_test",
            "validate_input",
        ];
        let clusters = cluster_by_name_segments(&names);

        // Should find clusters that group the extract_* and generate_* functions
        // The cluster name might be "extract", "signatures", "generate", etc.
        // depending on which segment is chosen as most specific
        let _extract_fns: Vec<&&str> = names[0..3].iter().collect();
        let _generate_fns: Vec<&&str> = names[3..6].iter().collect();

        // All 3 extract_* functions should be in the same cluster
        let extract_cluster = clusters
            .iter()
            .find(|(_, items)| items.contains(&"extract_php_signatures"));
        assert!(
            extract_cluster.is_some(),
            "extract_* functions should be clustered together"
        );
        let extract_items = &extract_cluster.unwrap().1;
        assert!(extract_items.contains(&"extract_rust_signatures"));
        assert!(extract_items.contains(&"extract_js_signatures"));

        // All 3 generate_* functions should be in the same cluster
        let generate_cluster = clusters
            .iter()
            .find(|(_, items)| items.contains(&"generate_stub"));
        assert!(
            generate_cluster.is_some(),
            "generate_* functions should be clustered together"
        );
        let generate_items = &generate_cluster.unwrap().1;
        assert!(generate_items.contains(&"generate_import"));
        assert!(generate_items.contains(&"generate_test"));
    }

    #[test]
    fn cluster_by_name_segments_unclustered_go_to_helpers() {
        let names = vec!["foo", "bar", "baz", "extract_a", "extract_b", "extract_c"];
        let clusters = cluster_by_name_segments(&names);

        let helpers = clusters.iter().find(|(name, _)| name == "helpers");
        assert!(helpers.is_some(), "Unclustered items should go to helpers");
        assert_eq!(helpers.unwrap().1.len(), 3); // foo, bar, baz
    }

    #[test]
    fn group_items_separates_types_from_functions() {
        let items = vec![
            item("Config", "struct"),
            item("Config", "impl"),
            item("Error", "enum"),
            item("load_config", "function"),
            item("save_config", "function"),
            item("validate_config", "function"),
        ];

        let groups = group_items("src/core/module.rs", &items, false);

        // Types and functions should be in separate groups
        let type_group = groups
            .iter()
            .find(|g| g.item_names.iter().any(|n| n == "Config" || n == "Error"));
        let fn_group = groups
            .iter()
            .find(|g| g.item_names.iter().any(|n| n == "load_config"));

        assert!(type_group.is_some(), "Should have a type group");
        assert!(fn_group.is_some(), "Should have a function group");

        // Types should not be in the function group
        let fn_group = fn_group.unwrap();
        assert!(
            !fn_group.item_names.contains(&"Config".to_string()),
            "Types should not leak into function groups"
        );
    }

    #[test]
    fn colocate_types_single_type() {
        let items = vec![item("Foo", "struct"), item("Foo", "impl")];
        let refs: Vec<&ParsedItem> = items.iter().collect();
        let groups = colocate_types(&refs);

        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].0, "types");
        assert_eq!(groups[0].1.len(), 2);
    }

    #[test]
    fn colocate_types_multiple_types() {
        let items = vec![
            item("Foo", "struct"),
            item("Foo", "impl"),
            item("Bar", "enum"),
            item("Display for Foo", "impl"),
        ];
        let refs: Vec<&ParsedItem> = items.iter().collect();
        let groups = colocate_types(&refs);

        // Should have separate groups for Foo and Bar
        assert!(groups.len() >= 2);

        let foo_group = groups
            .iter()
            .find(|(_, names)| names.contains(&"Foo".to_string()));
        assert!(foo_group.is_some());
        let foo_names = &foo_group.unwrap().1;
        assert!(
            foo_names.contains(&"Display for Foo".to_string()),
            "Trait impl should be co-located with the type"
        );
    }

    #[test]
    fn split_oversized_group_produces_subclusters() {
        let names: Vec<String> = (0..20)
            .map(|i| {
                if i < 7 {
                    format!("extract_item_{}", i)
                } else if i < 14 {
                    format!("generate_stub_{}", i)
                } else {
                    format!("helper_{}", i)
                }
            })
            .collect();

        let groups = split_oversized_group("big_group", &names);
        assert!(
            groups.len() > 1,
            "Should split into multiple sub-clusters, got {}",
            groups.len()
        );
    }

    #[test]
    fn to_snake_case_converts_pascal() {
        assert_eq!(to_snake_case("FixKind"), "fix_kind");
        assert_eq!(to_snake_case("PreflightReport"), "preflight_report");
        assert_eq!(to_snake_case("Fix"), "fix");
        assert_eq!(to_snake_case("ApplyChunkResult"), "apply_chunk_result");
    }

    #[test]
    fn stop_words_are_filtered() {
        assert!(is_stop_word("get"));
        assert!(is_stop_word("set"));
        assert!(is_stop_word("is"));
        assert!(is_stop_word("from"));
        assert!(!is_stop_word("extract"));
        assert!(!is_stop_word("generate"));
        assert!(!is_stop_word("validate"));
    }

    #[test]
    fn merge_small_groups_consolidates_tiny_groups() {
        let mut buckets: BTreeMap<String, Vec<String>> = BTreeMap::new();
        buckets.insert(
            "big_group".to_string(),
            vec!["a".to_string(), "b".to_string(), "c".to_string()],
        );
        buckets.insert("tiny".to_string(), vec!["x".to_string()]); // below threshold

        let merged = merge_small_groups(buckets);

        assert!(!merged.contains_key("tiny"), "Tiny group should be merged");
        assert!(
            merged.get("big_group").unwrap().contains(&"x".to_string()),
            "Tiny group items should be in the largest group"
        );
    }

    #[test]
    fn group_items_target_paths_use_file_stem() {
        let items = vec![
            item("foo", "function"),
            item("bar", "function"),
            item("baz", "function"),
        ];

        let groups = group_items("src/core/my_module.rs", &items, false);
        for g in &groups {
            assert!(
                g.suggested_target.starts_with("src/core/my_module/"),
                "Target should use file stem as directory: {}",
                g.suggested_target
            );
            assert!(
                g.suggested_target.ends_with(".rs"),
                "Non-audit-safe should use .rs extension"
            );
        }
    }

    #[test]
    fn group_items_audit_safe_uses_inc() {
        let items = vec![
            item("foo", "function"),
            item("bar", "function"),
            item("baz", "function"),
        ];

        let groups = group_items("src/core/big.rs", &items, true);
        for g in &groups {
            assert!(
                g.suggested_target.ends_with(".inc"),
                "Audit-safe should use .inc extension: {}",
                g.suggested_target
            );
        }
    }
}
