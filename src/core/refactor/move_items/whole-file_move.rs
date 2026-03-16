//! whole-file_move — extracted from move_items.rs.

use std::path::{Path, PathBuf};
use crate::core::engine::symbol_graph::module_path_from_file;
use crate::engine::codebase_scan::{self, ExtensionFilter, ScanConfig};
use crate::{component, Result};


/// Move an entire module file to a new location, rewriting all imports.
///
/// This is the `refactor move --file` operation:
/// 1. Move the file to the new path
/// 2. Remove `mod foo;` from the old parent's mod.rs
/// 3. Add `mod foo;` to the new parent's mod.rs
/// 4. Rewrite all `use crate::old::path::module` → `use crate::new::path::module` across the codebase
pub fn move_file(from: &str, to: &str, root: &Path, write: bool) -> Result<MoveFileResult> {
    let source_abs = root.join(from);
    let dest_abs = root.join(to);
    let warnings = Vec::new();

    // Validate source exists
    if !source_abs.exists() {
        return Err(crate::Error::validation_invalid_argument(
            "from",
            format!("Source file does not exist: {}", from),
            None,
            None,
        ));
    }

    // Validate destination doesn't exist
    if dest_abs.exists() {
        return Err(crate::Error::validation_invalid_argument(
            "to",
            format!("Destination already exists: {}", to),
            None,
            None,
        ));
    }

    // Compute module paths for import rewriting
    let source_module = module_path_from_file(from);
    let dest_module = module_path_from_file(to);

    // Derive module names for mod.rs updates
    let source_stem = Path::new(from)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    let dest_stem = Path::new(to)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("");

    // Find the extension for import rewriting
    let ext = find_refactor_extension(from);

    // ── Phase 1: Rewrite caller imports across the codebase ──────────
    let mut imports_updated: usize = 0;
    let mut caller_rewrites: Vec<(PathBuf, Vec<ImportRewrite>)> = Vec::new();

    if let Some(ref ext) = ext {
        if source_module != dest_module {
            // For a whole-module move, we need to find all files that import
            // anything from the source module. We use the module name as the
            // search term — any file that mentions it might have imports to update.
            let source_module_leaf = source_module.rsplit("::").next().unwrap_or(&source_module);

            let all_files = codebase_scan::walk_files(
                root,
                &ScanConfig {
                    extensions: ExtensionFilter::All,
                    ..Default::default()
                },
            );

            // Read the source file to find all pub item names — these are what
            // callers might import individually
            let source_content = std::fs::read_to_string(&source_abs).unwrap_or_default();
            let pub_item_names: Vec<String> = if let Some(items) =
                core_parse_items(ext, &source_content)
                    .or_else(|| ext_parse_items(ext, &source_content, from))
            {
                items
                    .iter()
                    .filter(|item| item.visibility == "pub" || item.visibility == "pub(crate)")
                    .map(|item| item.name.clone())
                    .collect()
            } else {
                vec![]
            };

            // Also include the module name itself for `use crate::path::module;` imports
            let mut search_terms: Vec<String> = pub_item_names.clone();
            search_terms.push(source_module_leaf.to_string());

            let search_refs: Vec<&str> = search_terms.iter().map(|s| s.as_str()).collect();

            for file_path in &all_files {
                let rel_path = file_path
                    .strip_prefix(root)
                    .unwrap_or(file_path)
                    .to_string_lossy()
                    .to_string();

                if rel_path == from || rel_path == to {
                    continue;
                }

                let file_ext_str = file_path.extension().and_then(|e| e.to_str()).unwrap_or("");
                if !ext.handles_file_extension(file_ext_str) {
                    continue;
                }

                let file_content = match std::fs::read_to_string(file_path) {
                    Ok(c) => c,
                    Err(_) => continue,
                };

                // Quick check: does this file mention the source module?
                let mentions_source = search_refs.iter().any(|term| file_content.contains(term));
                if !mentions_source {
                    continue;
                }

                if let Some(rewrites) = ext_rewrite_caller_imports(
                    ext,
                    &search_refs,
                    &source_module,
                    &dest_module,
                    &file_content,
                    &rel_path,
                ) {
                    if !rewrites.is_empty() {
                        imports_updated += rewrites.len();
                        caller_rewrites.push((file_path.to_path_buf(), rewrites));
                    }
                }
            }
        }
    }

    // ── Phase 2: Update mod.rs declarations ──────────────────────────
    let mut mod_declarations_updated = false;

    // Find old parent mod.rs and remove `mod source_stem;`
    let old_parent_mod = find_parent_mod_rs(from, root);
    let new_parent_mod = find_parent_mod_rs(to, root);

    // ── Phase 3: Apply if requested ─────────────────────────────────
    if write {
        // Create destination parent directory
        if let Some(parent) = dest_abs.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                crate::Error::internal_io(
                    e.to_string(),
                    Some(format!("create directory {}", parent.display())),
                )
            })?;
        }

        // Move the file
        std::fs::rename(&source_abs, &dest_abs).map_err(|e| {
            crate::Error::internal_io(e.to_string(), Some(format!("move {} → {}", from, to)))
        })?;

        // Update old parent mod.rs — remove mod declaration
        if source_stem != "mod" {
            if let Some(ref old_mod) = old_parent_mod {
                if old_mod.exists() {
                    if let Ok(content) = std::fs::read_to_string(old_mod) {
                        let new_content = remove_mod_declaration(&content, source_stem);
                        if new_content != content {
                            let _ = std::fs::write(old_mod, &new_content);
                            mod_declarations_updated = true;
                        }
                    }
                }
            }
        }

        // Update new parent mod.rs — add mod declaration
        if dest_stem != "mod" {
            if let Some(ref new_mod) = new_parent_mod {
                if new_mod.exists() {
                    if let Ok(content) = std::fs::read_to_string(new_mod) {
                        let new_content = add_mod_declaration(&content, dest_stem);
                        if new_content != content {
                            let _ = std::fs::write(new_mod, &new_content);
                            mod_declarations_updated = true;
                        }
                    }
                } else {
                    // Create mod.rs with just the new declaration
                    if let Some(parent) = new_mod.parent() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                    let _ = std::fs::write(new_mod, format!("mod {};\n", dest_stem));
                    mod_declarations_updated = true;
                }
            }
        }

        // Apply caller import rewrites
        for (file_path, rewrites) in &caller_rewrites {
            if let Ok(content) = std::fs::read_to_string(file_path) {
                let mut lines: Vec<&str> = content.split('\n').collect();
                // Apply rewrites in reverse line order to preserve line numbers
                let mut sorted_rewrites = rewrites.clone();
                sorted_rewrites.sort_by(|a, b| b.line.cmp(&a.line));
                for rewrite in &sorted_rewrites {
                    let idx = rewrite.line.saturating_sub(1);
                    if idx < lines.len() {
                        // Handle multi-line replacements (e.g., split grouped imports)
                        let replacement_lines: Vec<&str> =
                            rewrite.replacement.split('\n').collect();
                        lines.splice(idx..=idx, replacement_lines);
                    }
                }
                let new_content = lines.join("\n");
                let _ = std::fs::write(file_path, new_content);
            }
        }
    }

    let caller_files: Vec<String> = caller_rewrites
        .iter()
        .map(|(path, _)| {
            path.strip_prefix(root)
                .unwrap_or(path)
                .to_string_lossy()
                .to_string()
        })
        .collect();

    Ok(MoveFileResult {
        from_file: from.to_string(),
        to_file: to.to_string(),
        imports_updated,
        caller_files_modified: caller_files,
        applied: write,
        warnings,
        mod_declarations_updated,
    })
}

/// Find the parent module's mod.rs (or lib.rs) for a given file path.
pub(crate) fn find_parent_mod_rs(file_path: &str, root: &Path) -> Option<PathBuf> {
    let path = Path::new(file_path);
    let parent = path.parent()?;
    let mod_rs = root.join(parent).join("mod.rs");
    if mod_rs.exists() {
        return Some(mod_rs);
    }
    // Check for lib.rs in the parent (src/lib.rs)
    let lib_rs = root.join(parent).join("lib.rs");
    if lib_rs.exists() {
        return Some(lib_rs);
    }
    None
}

/// Remove a `mod foo;` declaration from module content.
pub(crate) fn remove_mod_declaration(content: &str, module_name: &str) -> String {
    let patterns = [
        format!("pub mod {};", module_name),
        format!("pub(crate) mod {};", module_name),
        format!("pub(super) mod {};", module_name),
        format!("mod {};", module_name),
    ];
    content
        .lines()
        .filter(|line| {
            let trimmed = line.trim();
            !patterns.iter().any(|p| trimmed == p)
        })
        .collect::<Vec<_>>()
        .join("\n")
        + if content.ends_with('\n') { "\n" } else { "" }
}

/// Add a `mod foo;` declaration to module content (after existing mod declarations).
pub(crate) fn add_mod_declaration(content: &str, module_name: &str) -> String {
    // Check if it already exists
    let patterns = [
        format!("pub mod {};", module_name),
        format!("pub(crate) mod {};", module_name),
        format!("mod {};", module_name),
    ];
    if content
        .lines()
        .any(|line| patterns.iter().any(|p| line.trim() == p))
    {
        return content.to_string();
    }

    // Find the last `mod` or `pub mod` line and insert after it
    let lines: Vec<&str> = content.lines().collect();
    let mut insert_after = None;
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if trimmed.starts_with("mod ")
            || trimmed.starts_with("pub mod ")
            || trimmed.starts_with("pub(crate) mod ")
        {
            insert_after = Some(i);
        }
    }

    let new_line = format!("pub mod {};", module_name);
    let mut result: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
    if let Some(idx) = insert_after {
        result.insert(idx + 1, new_line);
    } else {
        // No existing mod declarations — prepend
        result.insert(0, new_line);
        result.insert(1, String::new());
    }

    let mut out = result.join("\n");
    if content.ends_with('\n') && !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// Resolve the root directory from component ID or explicit path.
pub fn resolve_root(component_id: Option<&str>, path: Option<&str>) -> Result<PathBuf> {
    if let Some(p) = path {
        let pb = PathBuf::from(p);
        if !pb.is_dir() {
            return Err(crate::Error::validation_invalid_argument(
                "path",
                format!("Not a directory: {}", p),
                None,
                None,
            ));
        }
        Ok(pb)
    } else {
        let comp = component::resolve(component_id)?;
        let validated = component::validate_local_path(&comp)?;
        Ok(validated)
    }
}
