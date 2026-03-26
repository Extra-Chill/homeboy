//! group_refinement — extracted from decompose.rs.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use crate::core::scaffold::load_extension_grammar;
use crate::extension::grammar_items;
use crate::extension::{self, ParsedItem};
use crate::Result;
use super::super::move_items::{MoveOptions, MoveResult};
use serde::{Deserialize, Serialize};
use super::is_public_item_source;
use super::DecomposePlan;
use super::cluster_by_name_segments;
use super::is_effective_module_root;
use super::truncate_module_name;
use super::parse_items;


pub(crate) fn run_moves(plan: &DecomposePlan, root: &Path, write: bool) -> Result<Vec<MoveResult>> {
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
                // Decompose generates pub use re-exports in the source file,
                // so callers importing from the original module path still work.
                // Rewriting sibling imports would produce incorrect submodule paths.
                skip_caller_rewrites: true,
            },
        )?;
        results.push(result);
    }

    Ok(results)
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

pub(crate) fn identify_parent_kept_functions(
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

pub(crate) fn count_local_function_references(content: &str, name: &str) -> usize {
    let pattern = format!(r"\b{}\s*\(", regex::escape(name));
    let Ok(re) = regex::Regex::new(&pattern) else {
        return 0;
    };
    re.find_iter(content).count().saturating_sub(1)
}

pub(crate) fn is_root_orchestrator_name(name: &str) -> bool {
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
