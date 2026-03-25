mod call_graph_analysis;
mod constants;
mod group_refinement;
mod moves;
mod name_segment_clustering;
mod section_header_parsing;
mod source_test_file;
mod type_co_location;
mod types;

pub use name_segment_clustering::*;
pub use section_header_parsing::*;
pub use source_test_file::*;
pub use type_co_location::*;
pub use types::*;

use std::collections::{BTreeMap, HashSet};
// ============================================================================
// Call graph analysis
// ============================================================================

// ============================================================================
// Name segment clustering
// ============================================================================

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

// ============================================================================
// Main grouping algorithm
// ============================================================================

// ============================================================================
// Type co-location
// ============================================================================

// ============================================================================
// Group refinement
// ============================================================================

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
        let (components, hubs) = call_graph_components(&graph);

        // detect_drift only calls 2 functions, below HUB_THRESHOLD (4),
        // so it should NOT be a hub — it clusters with its callees
        assert!(
            !hubs.contains(&"detect_drift".to_string()),
            "detect_drift calls only 2 functions, should not be a hub"
        );

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
    fn call_graph_excludes_hubs_from_clusters() {
        // An orchestrator function that calls 5+ others should be identified as a hub
        // and excluded from union-find to prevent mega-clusters
        let items: Vec<ParsedItem> = vec![
            item_with_source(
                "orchestrate",
                "function",
                "fn orchestrate() { step_a(); step_b(); step_c(); step_d(); step_e(); }",
            ),
            item_with_source("step_a", "function", "fn step_a() { helper_a(); }"),
            item_with_source("helper_a", "function", "fn helper_a() {}"),
            item_with_source("step_b", "function", "fn step_b() {}"),
            item_with_source("step_c", "function", "fn step_c() {}"),
            item_with_source("step_d", "function", "fn step_d() {}"),
            item_with_source("step_e", "function", "fn step_e() {}"),
        ];
        let item_refs: Vec<&ParsedItem> = items.iter().collect();
        let fn_names: HashSet<&str> = items.iter().map(|i| i.name.as_str()).collect();

        let graph = build_call_graph(&item_refs, &fn_names);
        let (components, hubs) = call_graph_components(&graph);

        // orchestrate calls 5 functions → should be a hub
        assert!(
            hubs.contains(&"orchestrate".to_string()),
            "orchestrate should be identified as a hub (calls {} functions)",
            graph.get("orchestrate").map(|c| c.len()).unwrap_or(0)
        );

        // step_a and helper_a should cluster together (step_a calls helper_a)
        let step_a_component = components
            .iter()
            .find(|(_, members)| members.contains(&"step_a".to_string()));
        assert!(
            step_a_component.is_some(),
            "step_a + helper_a should form a cluster"
        );
        assert!(step_a_component
            .unwrap()
            .1
            .contains(&"helper_a".to_string()));

        // Without hub exclusion, all 7 functions would be in one mega-component.
        // With hub exclusion, we should have smaller, focused clusters.
        let max_cluster_size = components.iter().map(|(_, m)| m.len()).max().unwrap_or(0);
        assert!(
            max_cluster_size < 6,
            "No cluster should contain all non-hub functions (max: {})",
            max_cluster_size
        );
    }

    #[test]
    fn find_dominant_prefix_detects_shared_naming() {
        let members = vec![
            "resolve_assertion".to_string(),
            "resolve_constructor".to_string(),
            "resolve_type_default".to_string(),
        ];
        let prefix = find_dominant_prefix(&members);
        assert_eq!(prefix, Some("resolve".to_string()));

        let members = vec![
            "infer_setup_from_condition".to_string(),
            "infer_hint_for_param".to_string(),
            "infer_setup_with_complements".to_string(),
        ];
        let prefix = find_dominant_prefix(&members);
        // 2/3 members share "infer_setup" — more specific than "infer"
        assert_eq!(prefix, Some("infer_setup".to_string()));

        // No dominant prefix
        let members = vec!["foo".to_string(), "bar".to_string(), "baz".to_string()];
        let prefix = find_dominant_prefix(&members);
        assert_eq!(prefix, None);
    }

    #[test]
    fn section_name_to_slug_converts_headers() {
        assert_eq!(section_name_to_slug("Models"), "models");
        assert_eq!(section_name_to_slug("Git operations"), "git_operations");
        // Long headers are truncated to MAX_MODULE_NAME_WORDS meaningful words
        assert_eq!(
            section_name_to_slug("Diff parsing — extract structural changes"),
            "diff_parsing_extract"
        );
        assert_eq!(section_name_to_slug("Tests"), "tests");
    }

    #[test]
    fn section_name_to_slug_converts_hyphens_to_underscores() {
        // Hyphens are invalid in Rust module names
        assert_eq!(section_name_to_slug("Whole-file move"), "whole_file_move");
        assert_eq!(
            section_name_to_slug("re-export handling"),
            "re_export_handling"
        );
        assert_eq!(section_name_to_slug("pre-commit hooks"), "pre_commit_hooks");
    }

    #[test]
    fn sanitize_module_name_handles_invalid_chars() {
        assert_eq!(sanitize_module_name("whole-file_move"), "whole_file_move");
        assert_eq!(sanitize_module_name("foo.bar"), "foo_bar");
        assert_eq!(sanitize_module_name("valid_name"), "valid_name");
        assert_eq!(sanitize_module_name("a--b"), "a_b");
        assert_eq!(sanitize_module_name("types"), "types");
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

    #[test]
    fn group_items_mod_rs_uses_parent_dir_not_mod_subdir() {
        // When source is mod.rs, submodules should go in the same directory,
        // not in a "mod/" subdirectory. This is how Rust module resolution works.
        let items = vec![
            item("foo", "function"),
            item("bar", "function"),
            item("baz", "function"),
        ];

        let groups = group_items("src/core/code_audit/mod.rs", &items, "");
        for g in &groups {
            assert!(
                g.suggested_target.starts_with("src/core/code_audit/"),
                "Target should be in parent dir, not mod/ subdir: {}",
                g.suggested_target
            );
            assert!(
                !g.suggested_target.contains("/mod/"),
                "Target must NOT contain /mod/ directory: {}",
                g.suggested_target
            );
            assert!(
                g.suggested_target.ends_with(".rs"),
                "Should have .rs extension: {}",
                g.suggested_target
            );
        }
    }

    #[test]
    fn group_items_regular_file_uses_stem_subdir() {
        // Regular files (not mod.rs) should use the stem as a subdirectory
        let items = vec![
            item("foo", "function"),
            item("bar", "function"),
            item("baz", "function"),
        ];

        let groups = group_items("src/core/operations.rs", &items, "");
        for g in &groups {
            assert!(
                g.suggested_target.starts_with("src/core/operations/"),
                "Regular file should use stem as subdir: {}",
                g.suggested_target
            );
        }
    }

    #[test]
    fn truncate_module_name_limits_word_count() {
        // Verbose section headers should be truncated
        assert_eq!(
            truncate_module_name("structural_parser_context_aware_iteration_over_source_text"),
            "structural_parser_context"
        );
        assert_eq!(
            truncate_module_name("grammar_definition_loaded_from_extension_toml_json"),
            "grammar_definition_loaded"
        );
        assert_eq!(
            truncate_module_name("extraction_apply_grammar_patterns_to_get_symbols"),
            "extraction_apply_grammar"
        );
        assert_eq!(
            truncate_module_name("convenience_helpers_for_feature_consumers"),
            "convenience_helpers_feature"
        );
    }

    #[test]
    fn truncate_module_name_preserves_short_names() {
        assert_eq!(truncate_module_name("types"), "types");
        assert_eq!(truncate_module_name("block_syntax"), "block_syntax");
        assert_eq!(truncate_module_name("grammar_loading"), "grammar_loading");
        assert_eq!(truncate_module_name("symbol"), "symbol");
    }

    #[test]
    fn truncate_module_name_drops_stop_words() {
        // Stop words like "for", "from", "to", "in" are dropped, not counted
        assert_eq!(truncate_module_name("items_for_display"), "items_display");
        assert_eq!(
            truncate_module_name("data_from_source_to_target"),
            "data_source_target"
        );
    }
}
