//! moves — extracted from decompose.rs.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use crate::Result;
use super::super::move_items::{MoveOptions, MoveResult};
use serde::{Deserialize, Serialize};
use crate::extension::{self, ParsedItem};
use super::DecomposePlan;


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
    let results = run_moves(plan, root, true)?;

    // After all moves complete, generate module index (mod declarations + pub use
    // re-exports) in the source file. Without this, callers that imported from the
    // original module can't find the items that were moved to submodules.
    if results.iter().any(|r| r.applied) {
        generate_source_module_index(plan, root);
    }

    Ok(results)
}

/// Generate mod declarations and pub use re-exports in the source file after decompose.
///
/// The source file (now acting as mod.rs for its submodules) needs:
/// - `mod submodule;` declarations for each created submodule
/// - `pub use submodule::*;` re-exports so callers don't break
///
/// Delegates to the language extension's `generate_module_index` command for
/// language-specific syntax (Rust `pub use`, PHP `require_once`, etc.).
pub(crate) fn generate_source_module_index(plan: &DecomposePlan, root: &Path) {
    let source_path = root.join(&plan.file);

    // Read remaining content of the source file (items that weren't moved)
    let remaining_content = std::fs::read_to_string(&source_path).unwrap_or_default();

    // Build submodule entries from the plan groups
    let submodules: Vec<super::move_items::ModuleIndexEntry> = plan
        .groups
        .iter()
        .filter_map(|group| {
            // Derive module name from the target path
            let target = Path::new(&group.suggested_target);
            let stem = target.file_stem()?.to_str()?;
            Some(super::move_items::ModuleIndexEntry {
                name: stem.to_string(),
                pub_items: vec![], // empty = glob re-export (pub use submodule::*)
            })
        })
        .collect();

    if submodules.is_empty() {
        return;
    }

    // Remove use imports that would conflict with the new mod declarations.
    // When we add `mod grammar;`, any existing `use ...::grammar;` in the
    // remaining content would create "name defined multiple times" errors.
    let submodule_names: Vec<&str> = submodules.iter().map(|s| s.name.as_str()).collect();
    let cleaned_content = remove_conflicting_use_imports(&remaining_content, &submodule_names);

    if let Some(content) =
        super::move_items::ext_generate_module_index(&plan.file, &submodules, &cleaned_content)
    {
        if let Err(e) = std::fs::write(&source_path, content) {
            eprintln!(
                "Warning: failed to write module index to {}: {}",
                source_path.display(),
                e
            );
        }
    }
}

/// Remove `use` imports that would conflict with new `mod` declarations.
///
/// When decompose generates `mod foo;` + `pub use foo::*;`, any existing
/// `use some::path::foo;` in the remaining content introduces the name `foo`
/// twice. This function removes those conflicting imports.
pub(crate) fn remove_conflicting_use_imports(content: &str, submodule_names: &[&str]) -> String {
    content
        .lines()
        .filter(|line| {
            let trimmed = line.trim();
            // Only check use statements
            if !trimmed.starts_with("use ") && !trimmed.starts_with("pub use ") {
                return true;
            }
            // Check if this use statement brings a conflicting name into scope.
            // Patterns: `use path::name;` or `use path::name as _;`
            for name in submodule_names {
                // Simple tail import: `use something::name;`
                if trimmed.ends_with(&format!("::{};\n", name))
                    || trimmed.ends_with(&format!("::{};", name))
                {
                    return false;
                }
                // Grouped import containing the name: `use something::{name, other};`
                // Remove the whole line if it only imports the conflicting name,
                // otherwise leave it (partial removal is too complex for now).
                if trimmed.contains(&format!("::{{{}}}", name))
                    || trimmed.contains(&format!("{{ {} }}", name))
                {
                    return false;
                }
            }
            true
        })
        .collect::<Vec<_>>()
        .join("\n")
}

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
