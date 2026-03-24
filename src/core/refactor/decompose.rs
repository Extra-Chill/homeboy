mod call_graph_analysis;
mod constants;
mod group_refinement;
mod moves;
mod name_segment_clustering;
mod section_header_parsing;
mod source_test_file;
mod type_co_location;
mod types;

pub use call_graph_analysis::*;
pub use constants::*;
pub use group_refinement::*;
pub use moves::*;
pub use name_segment_clustering::*;
pub use section_header_parsing::*;
pub use source_test_file::*;
pub use type_co_location::*;
pub use types::*;

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::core::scaffold::load_extension_grammar;
use crate::extension::grammar_items;
use crate::extension::{self, ParsedItem};
use crate::Result;

use super::move_items::{MoveOptions, MoveResult};

// ============================================================================
// Section header parsing
// ============================================================================

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

// ============================================================================
// Diff parsing
// ============================================================================

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
// === Rendering ===
"#;
        let sections = extract_sections(content);
        assert_eq!(sections.len(), 3);
        assert_eq!(sections[0].name, "types");
        assert_eq!(sections[1].name, "parsing");
        assert_eq!(sections[2].name, "rendering");
    }

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
