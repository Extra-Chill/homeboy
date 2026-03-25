//! reference_finding — extracted from mod.rs.

use crate::engine::codebase_scan::{
    self, find_boundary_matches, find_case_insensitive_matches, find_literal_matches,
    ExtensionFilter, ScanConfig,
};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use crate::error::{Error, Result};
use serde::Serialize;
use crate::core::refactor::rename::target_files;
use crate::core::refactor::rename::RenameSpec;
use crate::core::refactor::rename::Reference;
use crate::core::refactor::rename::RenameTargeting;
use crate::core::refactor::rename::CaseVariant;


/// Find all references to the rename term across the codebase.
///
/// After the initial pass, discovers additional case variants that exist in the
/// codebase but weren't generated (e.g., `WPAgent` when `WpAgent` was generated).
pub fn find_references(spec: &RenameSpec, root: &Path) -> Vec<Reference> {
    find_references_with_targeting(spec, root, &RenameTargeting::default())
}

/// Find references using optional include/exclude targeting controls.
pub fn find_references_with_targeting(
    spec: &RenameSpec,
    root: &Path,
    targeting: &RenameTargeting,
) -> Vec<Reference> {
    let config = scan_config_for_scope(&spec.scope);
    let files = target_files(codebase_scan::walk_files(root, &config), root, targeting);

    // Build the working variant list — may be extended by discovery
    let mut all_variants = spec.variants.clone();

    // Initial search with generated variants.
    if !spec.literal {
        // Discover additional case variants for any generated variant with 0 matches.
        // For example, if we generated "WpAgent" but the codebase uses "WPAgent",
        // the case-insensitive scan will find it and add it as a discovered variant.
        let mut discovered = Vec::new();
        for variant in &spec.variants {
            // Quick check: does this variant have any boundary matches?
            let has_matches = files.iter().any(|f| {
                std::fs::read_to_string(f)
                    .map(|content| {
                        content
                            .lines()
                            .any(|line| !find_boundary_matches(line, &variant.from).is_empty())
                    })
                    .unwrap_or(false)
            });

            if !has_matches {
                // No matches — discover what casing actually exists
                let casings = discover_casing_in_files(&files, &variant.from, spec.literal);
                for (actual_casing, _count) in &casings {
                    // Skip if it's the same as what we already have
                    if actual_casing == &variant.from {
                        continue;
                    }
                    // Skip if we already have this variant
                    if all_variants.iter().any(|v| v.from == *actual_casing) {
                        continue;
                    }
                    discovered.push(CaseVariant {
                        from: actual_casing.clone(),
                        to: variant.to.clone(),
                        label: format!("discovered ({})", variant.label),
                    });
                }
            }
        }
        all_variants.extend(discovered);
    }

    // Phase 2: Find all references using the full variant list
    let mut references = Vec::new();

    // Sort variants longest-first to prevent partial overlap
    all_variants.sort_by(|a, b| b.from.len().cmp(&a.from.len()));

    let use_literal = spec.literal;

    for file_path in &files {
        let Ok(content) = std::fs::read_to_string(file_path) else {
            continue;
        };

        let relative = file_path
            .strip_prefix(root)
            .unwrap_or(file_path)
            .to_string_lossy()
            .to_string();

        for (line_num, line) in content.lines().enumerate() {
            // Track which byte offsets in this line are already claimed
            // to prevent overlapping matches from shorter variants
            let mut claimed: Vec<(usize, usize)> = Vec::new();

            for variant in &all_variants {
                let positions = if use_literal {
                    find_literal_matches(line, &variant.from)
                } else {
                    find_boundary_matches(line, &variant.from)
                };
                for pos in positions {
                    let end = pos + variant.from.len();
                    // Skip if this range overlaps with an already-claimed match
                    if claimed.iter().any(|&(s, e)| pos < e && end > s) {
                        continue;
                    }
                    // Apply syntactic context filter
                    if !spec.rename_context.matches(line, pos, variant.from.len()) {
                        continue;
                    }
                    claimed.push((pos, end));
                    references.push(Reference {
                        file: relative.clone(),
                        line: line_num + 1,
                        column: pos + 1,
                        matched: variant.from.clone(),
                        replacement: variant.to.clone(),
                        variant: variant.label.clone(),
                        context: line.to_string(),
                    });
                }
            }
        }
    }

    references
}

pub(crate) fn discover_casing_in_files(
    files: &[PathBuf],
    pattern: &str,
    literal: bool,
) -> Vec<(String, usize)> {
    let mut counts: HashMap<String, usize> = HashMap::new();

    for file in files {
        let Ok(content) = std::fs::read_to_string(file) else {
            continue;
        };

        for line in content.lines() {
            if literal {
                for pos in find_literal_matches(line, pattern) {
                    let matched = &line[pos..pos + pattern.len()];
                    *counts.entry(matched.to_string()).or_insert(0) += 1;
                }
                continue;
            }

            for (_start, matched) in find_case_insensitive_matches(line, pattern) {
                *counts.entry(matched).or_insert(0) += 1;
            }
        }
    }

    let mut out: Vec<(String, usize)> = counts.into_iter().collect();
    out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    out
}
