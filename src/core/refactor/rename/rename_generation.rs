//! rename_generation — extracted from mod.rs.

use crate::engine::codebase_scan::{
    self, find_boundary_matches, find_case_insensitive_matches, find_literal_matches,
    ExtensionFilter, ScanConfig,
};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use crate::error::{Error, Result};
use serde::Serialize;
use crate::core::refactor::rename::FileRename;
use crate::core::refactor::rename::RenameContext;
use crate::core::refactor::rename::RenameTargeting;
use crate::core::refactor::rename::RenameSpec;
use crate::core::refactor::rename::FileEdit;
use crate::core::refactor::rename::RenameResult;


/// Generate file edits and file renames from found references.
pub fn generate_renames(spec: &RenameSpec, root: &Path) -> RenameResult {
    generate_renames_with_targeting(spec, root, &RenameTargeting::default())
}

/// Generate renames using optional include/exclude targeting controls.
pub fn generate_renames_with_targeting(
    spec: &RenameSpec,
    root: &Path,
    targeting: &RenameTargeting,
) -> RenameResult {
    let references = find_references_with_targeting(spec, root, targeting);
    let config = scan_config_for_scope(&spec.scope);
    let files = target_files(codebase_scan::walk_files(root, &config), root, targeting);

    // Sort variants longest-first to prevent partial matches
    let mut sorted_variants = spec.variants.clone();
    sorted_variants.sort_by(|a, b| b.from.len().cmp(&a.from.len()));

    // Generate file content edits using reverse-offset replacement
    let mut edits = Vec::new();
    let mut affected_files: HashMap<String, bool> = HashMap::new();
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

        // Collect all matches with their positions and replacements
        let mut all_matches: Vec<(usize, usize, String)> = Vec::new(); // (start, end, replacement)

        for variant in &sorted_variants {
            let positions = if use_literal {
                find_literal_matches(&content, &variant.from)
            } else {
                find_boundary_matches(&content, &variant.from)
            };
            for pos in positions {
                let end = pos + variant.from.len();
                // Skip if overlapping with an already-claimed longer match
                if all_matches.iter().any(|&(s, e, _)| pos < e && end > s) {
                    continue;
                }
                // Apply syntactic context filter on the line containing this match
                if spec.rename_context != RenameContext::All {
                    let line_start = content[..pos].rfind('\n').map_or(0, |p| p + 1);
                    let line_end = content[pos..].find('\n').map_or(content.len(), |p| pos + p);
                    let line = &content[line_start..line_end];
                    let col = pos - line_start;
                    if !spec.rename_context.matches(line, col, variant.from.len()) {
                        continue;
                    }
                }
                all_matches.push((pos, end, variant.to.clone()));
            }
        }

        if !all_matches.is_empty() {
            let count = all_matches.len();

            // Sort by position descending so we can replace from end to start
            // without invalidating earlier offsets
            all_matches.sort_by(|a, b| b.0.cmp(&a.0));

            let mut new_content = content;
            for (start, end, replacement) in &all_matches {
                new_content.replace_range(start..end, replacement);
            }

            affected_files.insert(relative.clone(), true);
            edits.push(FileEdit {
                file: relative,
                replacements: count,
                new_content,
            });
        }
    }

    // Generate file/directory renames
    let mut file_renames = Vec::new();
    if targeting.rename_files {
        for file_path in &files {
            let relative = file_path
                .strip_prefix(root)
                .unwrap_or(file_path)
                .to_string_lossy()
                .to_string();

            let mut new_relative = relative.clone();
            for variant in &sorted_variants {
                // Replace in path segments (word-boundary aware in file names)
                new_relative = new_relative.replace(&variant.from, &variant.to);
            }

            if new_relative != relative {
                file_renames.push(FileRename {
                    from: relative,
                    to: new_relative,
                });
            }
        }
    }

    // Deduplicate file renames
    file_renames.dedup_by(|a, b| a.from == b.from);

    let total_references = references.len();
    let total_files = affected_files.len() + file_renames.len();

    // Detect collisions
    let warnings = detect_collisions(&edits, &file_renames, root);

    RenameResult {
        variants: spec.variants.clone(),
        references,
        edits,
        file_renames,
        warnings,
        total_references,
        total_files,
        applied: false,
    }
}

pub(crate) fn target_files(files: Vec<PathBuf>, root: &Path, targeting: &RenameTargeting) -> Vec<PathBuf> {
    files
        .into_iter()
        .filter(|file| {
            let relative = file
                .strip_prefix(root)
                .unwrap_or(file)
                .to_string_lossy()
                .replace('\\', "/");

            if !targeting.include_globs.is_empty()
                && !targeting
                    .include_globs
                    .iter()
                    .any(|glob| glob_match::glob_match(glob, &relative))
            {
                return false;
            }

            if targeting
                .exclude_globs
                .iter()
                .any(|glob| glob_match::glob_match(glob, &relative))
            {
                return false;
            }

            true
        })
        .collect()
}
