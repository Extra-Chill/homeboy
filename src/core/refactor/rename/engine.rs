//! Rename engine — reference finding, edit/rename generation, and application.

use crate::core::engine::codebase_scan::{
    self, find_boundary_matches, find_case_insensitive_matches, find_literal_matches,
    ExtensionFilter, ScanConfig,
};
use crate::core::error::{Error, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::safety::detect_collisions;
use super::types::{
    CaseVariant, FileEdit, FileRename, Reference, RenameContext, RenameResult, RenameScope,
    RenameSpec, RenameTargeting,
};

// Boundary matching and literal matching are provided by crate::core::engine::codebase_scan.
// See: find_boundary_matches(), find_literal_matches()

// ============================================================================
// File walking — delegates to crate::core::engine::codebase_scan
// ============================================================================

/// Build a ScanConfig appropriate for rename operations.
fn scan_config_for_scope(scope: &RenameScope) -> ScanConfig {
    let extensions = match scope {
        RenameScope::Code => ExtensionFilter::Except(vec![
            "json".to_string(),
            "toml".to_string(),
            "yaml".to_string(),
            "yml".to_string(),
        ]),
        RenameScope::Config => ExtensionFilter::Only(vec![
            "json".to_string(),
            "toml".to_string(),
            "yaml".to_string(),
            "yml".to_string(),
        ]),
        RenameScope::All => ExtensionFilter::SourceDefaults,
    };

    ScanConfig {
        extensions,
        ..ScanConfig::default()
    }
}

// ============================================================================
// Reference finding
// ============================================================================

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
    // When --files globs are provided, bypass the default extension filter
    // so any file type can be targeted explicitly.
    let config = if !targeting.include_globs.is_empty() {
        ScanConfig {
            extensions: ExtensionFilter::All,
            ..ScanConfig::default()
        }
    } else {
        scan_config_for_scope(&spec.scope)
    };
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

fn discover_casing_in_files(
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

// ============================================================================
// Rename generation
// ============================================================================

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

fn target_files(files: Vec<PathBuf>, root: &Path, targeting: &RenameTargeting) -> Vec<PathBuf> {
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

// ============================================================================
// Apply renames
// ============================================================================

/// Apply rename edits and file renames to disk.
///
/// Content edits are written directly (whole-file replacement). File/directory
/// renames are routed through `apply_edit_ops()` so they share the same
/// execution path as the fixer pipeline and other manual commands.
pub fn apply_renames(result: &mut RenameResult, root: &Path) -> Result<()> {
    use crate::core::engine::edit_op::rename_file_moves_to_edit_ops;
    use crate::core::engine::edit_op_apply::apply_edit_ops;

    // Apply content edits first (whole-file replacement — rename operates at
    // full-content granularity, not line-level, so these bypass EditOp).
    for edit in &result.edits {
        let path = root.join(&edit.file);
        std::fs::write(&path, &edit.new_content).map_err(|e| {
            Error::internal_io(e.to_string(), Some(format!("write {}", path.display())))
        })?;
    }

    // Apply file/directory renames through the shared EditOp engine.
    // apply_edit_ops() handles parent directory creation and error reporting.
    if !result.file_renames.is_empty() {
        // Sort by path depth descending so children rename before parents.
        let mut sorted_renames = result.file_renames.clone();
        sorted_renames.sort_by(|a, b| {
            b.from
                .matches('/')
                .count()
                .cmp(&a.from.matches('/').count())
        });

        // Temporarily replace file_renames with the sorted order for conversion.
        let original_renames = std::mem::replace(&mut result.file_renames, sorted_renames);
        let ops = rename_file_moves_to_edit_ops(result);
        result.file_renames = original_renames;

        let report = apply_edit_ops(&ops, root).map_err(|e| {
            Error::internal_io(
                format!("apply_edit_ops failed for file renames: {}", e),
                Some("rename.apply".to_string()),
            )
        })?;

        if !report.errors.is_empty() {
            let error_messages: Vec<String> = report
                .errors
                .iter()
                .map(|e| format!("{}: {}", e.file, e.message))
                .collect();
            return Err(Error::internal_io(
                format!("File rename errors: {}", error_messages.join("; ")),
                Some("rename.apply".to_string()),
            ));
        }
    }

    result.applied = true;
    Ok(())
}
