use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::core::scaffold::load_extension_grammar;
use crate::extension::{self, ParsedItem};
use crate::extension::grammar_items;
use crate::Result;

use super::move_items::MoveOptions;
use super::MoveResult;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecomposePlan {
    pub file: String,
    pub strategy: String,
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

pub fn build_plan(file: &str, root: &Path, strategy: &str) -> Result<DecomposePlan> {
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

    let groups = group_items(file, &items, &content);
    let projected_audit_impact = project_audit_impact(&groups);

    let checklist = vec![
        "Review grouping and target filenames".to_string(),
        "Review projected audit impact before applying".to_string(),
        "Apply grouped extraction in one deterministic pass (homeboy refactor decompose --write)"
            .to_string(),
        "Run cargo test and homeboy audit --changed-since origin/main".to_string(),
    ];

    Ok(DecomposePlan {
        file: file.to_string(),
        strategy: strategy.to_string(),
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

fn project_audit_impact(groups: &[DecomposeGroup]) -> DecomposeAuditImpact {
    let mut likely_findings = Vec::new();
    let mut recommended_test_files = Vec::new();

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
            "New src/*.rs targets will need matching tests (autofix handles this)".to_string(),
        );
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

/// Maximum items per group before we attempt to split further.
const MAX_GROUP_SIZE: usize = 15;

/// Groups below this size get merged into the nearest related group.
const MERGE_THRESHOLD: usize = 2;

/// Minimum number of items sharing a word to form a name-based cluster.
const MIN_CLUSTER_SIZE: usize = 2;

// ============================================================================
// Section header parsing
// ============================================================================

/// A section header found in source comments (e.g., `// === Models ===`).
#[derive(Debug)]
struct Section {
    name: String,
    start_line: usize,
}

/// Extract section headers from file content.
///
/// Recognizes patterns like:
/// - `// === Section Name ===`
/// - `// --- Section Name ---`
/// - `// *** Section Name ***`
/// - `// Section Name` (preceded by a blank line + separator comment)
fn extract_sections(content: &str) -> Vec<Section> {
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
fn section_name_to_slug(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == ' ' || c == '_' || c == '-' {
                c
            } else {
                ' '
            }
        })
        .collect();

    cleaned
        .split_whitespace()
        .map(|w| w.to_lowercase())
        .collect::<Vec<_>>()
        .join("_")
}

/// Assign an item to a section based on its line number.
fn find_section_for_item(sections: &[Section], item_start_line: usize) -> Option<&str> {
    // Find the last section whose start_line is <= item_start_line
    sections
        .iter()
        .rev()
        .find(|s| s.start_line <= item_start_line)
        .map(|s| s.name.as_str())
}

// ============================================================================
// Call graph analysis
// ============================================================================

/// Build a map of which functions call which other functions in the same file.
///
/// Returns: caller -> set of callees (only functions that exist in `fn_names`).
fn build_call_graph(
    fn_items: &[&ParsedItem],
    fn_names: &HashSet<&str>,
) -> BTreeMap<String, HashSet<String>> {
    let mut graph: BTreeMap<String, HashSet<String>> = BTreeMap::new();

    for item in fn_items {
        let mut callees = HashSet::new();
        for name in fn_names {
            if *name != item.name && item.source.contains(name) {
                // Verify it's a real call reference (not just a substring match)
                // by checking for word boundaries around the name
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

/// Find connected components in the call graph using union-find.
///
/// Returns groups of function names that are connected by call relationships.
fn call_graph_components(graph: &BTreeMap<String, HashSet<String>>) -> Vec<(String, Vec<String>)> {
    // Union-find: map each name to its root
    let all_names: Vec<String> = graph.keys().cloned().collect();
    let mut parent: BTreeMap<String, String> = BTreeMap::new();
    for name in &all_names {
        parent.insert(name.clone(), name.clone());
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

    // Union functions that call each other
    for (caller, callees) in graph {
        for callee in callees {
            if parent.contains_key(callee) {
                union(&mut parent, caller, callee);
            }
        }
    }

    // Group by root
    let mut groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for name in &all_names {
        let root = find(&mut parent, name);
        groups.entry(root).or_default().push(name.clone());
    }

    // Only return groups with 2+ members (singletons aren't useful)
    groups
        .into_iter()
        .filter(|(_, members)| members.len() >= 2)
        .collect()
}

/// Pick a representative name for a call-graph cluster.
///
/// Prefers the function that is called by the most others (the "hub"),
/// or falls back to the first alphabetically.
fn pick_cluster_label(members: &[String], graph: &BTreeMap<String, HashSet<String>>) -> String {
    // Count how many times each member is called by other members
    let mut call_count: BTreeMap<&str, usize> = BTreeMap::new();
    for member in members {
        for callee in graph.get(member).into_iter().flatten() {
            if members.iter().any(|m| m == callee) {
                *call_count.entry(callee.as_str()).or_default() += 1;
            }
        }
    }

    // Pick the most-called member as the label
    call_count
        .into_iter()
        .max_by_key(|(_, count)| *count)
        .map(|(name, _)| name.to_string())
        .unwrap_or_else(|| {
            members
                .first()
                .cloned()
                .unwrap_or_else(|| "group".to_string())
        })
}

// ============================================================================
// Name segment clustering
// ============================================================================

/// Split a function name into semantic segments by `_`.
fn name_segments(name: &str) -> Vec<String> {
    name.split('_')
        .filter(|s| !s.is_empty() && s.len() > 1) // skip single-char segments
        .map(|s| s.to_lowercase())
        .collect()
}

/// Generate multi-word prefixes from a name (e.g., "extract_changes_from_diff" → ["extract_changes", "extract"]).
fn name_prefixes(name: &str) -> Vec<String> {
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
fn cluster_by_name_segments<'a>(names: &[&'a str]) -> Vec<(String, Vec<&'a str>)> {
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

    // Sort by specificity: longer prefixes first, then by count
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

    // Pass 2: Single segment clustering for remaining names
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

        // Sort by specificity: longer segments first
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

    // Collect truly unclustered items
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

// ============================================================================
// Main grouping algorithm
// ============================================================================

fn group_items(file: &str, items: &[ParsedItem], content: &str) -> Vec<DecomposeGroup> {
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
        let components = call_graph_components(&call_graph);
        let mut graph_assigned: HashSet<String> = HashSet::new();

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

        // Remaining: name-based clustering
        let still_unassigned: Vec<&str> = unassigned_fns
            .iter()
            .map(|i| i.name.as_str())
            .filter(|n| !graph_assigned.contains(*n))
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

// ============================================================================
// Type co-location
// ============================================================================

/// Group type items (struct/enum/trait + their impls) together.
///
/// If there's only one type, everything goes in "types". If there are multiple,
/// each type gets its own group named after it (snake_case).
fn colocate_types(items: &[&ParsedItem]) -> Vec<(String, Vec<String>)> {
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

// ============================================================================
// Group refinement
// ============================================================================

/// Merge groups with fewer than MERGE_THRESHOLD items into the nearest relative.
///
/// Type groups (tracked by `type_keys`) are protected: they only merge into
/// other type groups, never into function groups. This prevents types from
/// leaking into function clusters when deduplication shrinks a type group.
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
fn merge_small_groups(buckets: BTreeMap<String, Vec<String>>) -> BTreeMap<String, Vec<String>> {
    merge_small_groups_protected(buckets, &HashSet::new())
}

/// Split an oversized group into sub-groups using name clustering.
fn split_oversized_group(name: &str, names: &[String]) -> Vec<(String, Vec<String>)> {
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

        let groups = group_items("src/core/module.rs", &items, "");

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
        let items = [item("Foo", "struct"), item("Foo", "impl")];
        let refs: Vec<&ParsedItem> = items.iter().collect();
        let groups = colocate_types(&refs);

        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].0, "types");
        assert_eq!(groups[0].1.len(), 2);
    }

    #[test]
    fn colocate_types_multiple_types() {
        let items = [
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

        let groups = group_items("src/core/my_module.rs", &items, "");
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
    fn group_items_preserves_source_extension() {
        let items = vec![
            item("foo", "function"),
            item("bar", "function"),
            item("baz", "function"),
        ];

        let groups = group_items("src/core/big.rs", &items, "");
        for g in &groups {
            assert!(
                g.suggested_target.ends_with(".rs"),
                "Should preserve .rs extension: {}",
                g.suggested_target
            );
        }
    }

    #[test]
    fn extract_sections_from_separator_headers() {
        let content = r#"
use something;

// ============================================================================
// Models
// ============================================================================

pub struct Foo {}

// ============================================================================
// Git operations
// ============================================================================

fn git_fetch() {}

// ============================================================================
// Diff parsing
// ============================================================================

fn parse_diff() {}
"#;
        let sections = extract_sections(content);
        assert_eq!(sections.len(), 3);
        assert_eq!(sections[0].name, "models");
        assert_eq!(sections[1].name, "git_operations");
        assert_eq!(sections[2].name, "diff_parsing");
    }

    #[test]
    fn extract_sections_from_inline_headers() {
        let content = r#"
// === Types ===
struct A {}

// === Parsing ===
fn parse() {}

// === Rendering ===
fn render() {}
"#;
        let sections = extract_sections(content);
        assert_eq!(sections.len(), 3);
        assert_eq!(sections[0].name, "types");
        assert_eq!(sections[1].name, "parsing");
        assert_eq!(sections[2].name, "rendering");
    }

    #[test]
    fn section_headers_guide_function_grouping() {
        let content = r#"
// ============================================================================
// Git operations
// ============================================================================

fn get_changed_files() {}
fn get_renamed_files() {}

// ============================================================================
// Diff parsing
// ============================================================================

fn extract_changes_from_diff() {}
fn parse_hunk() {}
"#;
        let items = vec![
            item_at("get_changed_files", "function", 5, 5),
            item_at("get_renamed_files", "function", 6, 6),
            item_at("extract_changes_from_diff", "function", 12, 12),
            item_at("parse_hunk", "function", 13, 13),
        ];

        let groups = group_items("src/core/drift.rs", &items, content);

        let git_group = groups
            .iter()
            .find(|g| g.item_names.contains(&"get_changed_files".to_string()));
        assert!(git_group.is_some(), "Should have a git group");
        let git_items = &git_group.unwrap().item_names;
        assert!(
            git_items.contains(&"get_renamed_files".to_string()),
            "Git functions should be in same section group"
        );

        let diff_group = groups.iter().find(|g| {
            g.item_names
                .contains(&"extract_changes_from_diff".to_string())
        });
        assert!(diff_group.is_some(), "Should have a diff group");
        let diff_items = &diff_group.unwrap().item_names;
        assert!(
            diff_items.contains(&"parse_hunk".to_string()),
            "Diff functions should be in same section group"
        );

        // The two groups should be different
        assert_ne!(
            git_group.unwrap().name,
            diff_group.unwrap().name,
            "Git and diff groups should be separate"
        );
    }

    fn item_at(name: &str, kind: &str, start: usize, end: usize) -> ParsedItem {
        ParsedItem {
            name: name.to_string(),
            kind: kind.to_string(),
            start_line: start,
            end_line: end,
            source: String::new(),
            visibility: String::new(),
        }
    }

    fn item_with_source(name: &str, kind: &str, source: &str) -> ParsedItem {
        ParsedItem {
            name: name.to_string(),
            kind: kind.to_string(),
            start_line: 1,
            end_line: 10,
            source: source.to_string(),
            visibility: String::new(),
        }
    }

    #[test]
    fn call_graph_clusters_related_functions() {
        let items: Vec<ParsedItem> = vec![
            item_with_source(
                "detect_drift",
                "function",
                "fn detect_drift() { get_changed_files(); extract_changes_from_diff(); }",
            ),
            item_with_source("get_changed_files", "function", "fn get_changed_files() {}"),
            item_with_source(
                "extract_changes_from_diff",
                "function",
                "fn extract_changes_from_diff() {}",
            ),
            item_with_source(
                "generate_rules",
                "function",
                "fn generate_rules() { is_auto_fixable(); }",
            ),
            item_with_source("is_auto_fixable", "function", "fn is_auto_fixable() {}"),
        ];
        let item_refs: Vec<&ParsedItem> = items.iter().collect();
        let fn_names: HashSet<&str> = items.iter().map(|i| i.name.as_str()).collect();

        let graph = build_call_graph(&item_refs, &fn_names);
        let components = call_graph_components(&graph);

        // detect_drift calls get_changed_files and extract_changes_from_diff → one component
        let detect_component = components
            .iter()
            .find(|(_, members)| members.contains(&"detect_drift".to_string()));
        assert!(
            detect_component.is_some(),
            "detect_drift group should exist"
        );
        let members = &detect_component.unwrap().1;
        assert!(members.contains(&"get_changed_files".to_string()));
        assert!(members.contains(&"extract_changes_from_diff".to_string()));

        // generate_rules calls is_auto_fixable → separate component
        let rules_component = components
            .iter()
            .find(|(_, members)| members.contains(&"generate_rules".to_string()));
        assert!(rules_component.is_some(), "rules group should exist");
        let members = &rules_component.unwrap().1;
        assert!(members.contains(&"is_auto_fixable".to_string()));
    }

    #[test]
    fn section_name_to_slug_converts_headers() {
        assert_eq!(section_name_to_slug("Models"), "models");
        assert_eq!(section_name_to_slug("Git operations"), "git_operations");
        assert_eq!(
            section_name_to_slug("Diff parsing — extract structural changes"),
            "diff_parsing_extract_structural_changes"
        );
        assert_eq!(section_name_to_slug("Tests"), "tests");
    }

    #[test]
    fn name_prefixes_generates_multi_word() {
        let prefixes = name_prefixes("extract_changes_from_diff");
        assert!(prefixes.contains(&"extract_changes".to_string()));
        assert!(prefixes.contains(&"extract".to_string()));

        let prefixes = name_prefixes("foo");
        assert!(prefixes.contains(&"foo".to_string()));
        assert_eq!(prefixes.len(), 1);
    }

    #[test]
    fn cluster_with_min_size_two() {
        // With MIN_CLUSTER_SIZE=2, even pairs should cluster
        let names = vec![
            "parse_header",
            "parse_body",
            "render_output",
            "validate_input",
        ];
        let clusters = cluster_by_name_segments(&names);

        let parse_cluster = clusters
            .iter()
            .find(|(_, items)| items.contains(&"parse_header"));
        assert!(
            parse_cluster.is_some(),
            "parse_* pair should cluster together"
        );
        assert!(parse_cluster.unwrap().1.contains(&"parse_body"));
    }
}
