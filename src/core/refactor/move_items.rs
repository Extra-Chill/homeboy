//! Refactor move — extract items from one file and move them to another.
//!
//! Language-agnostic orchestration layer. All language-specific parsing
//! (item location, import resolution, visibility adjustment, test detection)
//! is delegated to extension refactor scripts.
//!
//! Extensions implement the `scripts.refactor` protocol, receiving JSON commands
//! on stdin and returning JSON results on stdout. When no extension is available
//! for a file type, move operates in fallback mode (basic line-range extraction).
//!
//! Usage:
//!   `homeboy refactor move --item "has_import" --from src/code_audit/conventions.rs --to src/code_audit/import_matching.rs`

use std::path::{Path, PathBuf};

use crate::core::engine::symbol_graph::module_path_from_file;
use crate::core::scaffold::load_extension_grammar;
use crate::engine::codebase_scan::{self, ExtensionFilter, ScanConfig};
use crate::extension::grammar_items;
use crate::extension::{
    self, AdjustedItem, ExtensionManifest, ParsedItem, RelatedTests, ResolvedImports,
};
use crate::{component, Result};

/// Result of a move operation.
#[derive(Debug, Clone, serde::Serialize)]
pub struct MoveResult {
    /// Items that were moved.
    pub items_moved: Vec<MovedItem>,
    /// The source file items were extracted from.
    pub from_file: String,
    /// The destination file items were moved to.
    pub to_file: String,
    /// Whether the destination file was created (vs. appended to).
    pub file_created: bool,
    /// Number of import references updated across the codebase.
    pub imports_updated: usize,
    /// Absolute paths of caller files whose imports were rewritten.
    /// Used by decompose rollback to restore these files if the move is reverted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub caller_files_modified: Vec<PathBuf>,
    /// Related tests that were moved alongside items.
    pub tests_moved: Vec<MovedItem>,
    /// Whether changes were written to disk.
    pub applied: bool,
    /// Warnings generated during the move.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// Behavioral options for move operations.
#[derive(Debug, Clone, Copy)]
pub struct MoveOptions {
    /// Whether related test functions should be moved alongside requested items.
    pub move_related_tests: bool,
}

impl Default for MoveOptions {
    fn default() -> Self {
        Self {
            move_related_tests: true,
        }
    }
}

/// A single item that was moved.
#[derive(Debug, Clone, serde::Serialize)]
pub struct MovedItem {
    /// Name of the item (function, struct, etc.).
    pub name: String,
    /// What kind of item.
    pub kind: ItemKind,
    /// Line range in the source file (1-indexed, inclusive).
    pub source_lines: (usize, usize),
    /// Number of lines (including doc comments and attributes).
    pub line_count: usize,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemKind {
    Function,
    Struct,
    Enum,
    Const,
    Static,
    TypeAlias,
    Impl,
    Trait,
    Test,
    Unknown,
}

impl ItemKind {
    fn from_str(s: &str) -> Self {
        match s {
            "function" => ItemKind::Function,
            "struct" => ItemKind::Struct,
            "enum" => ItemKind::Enum,
            "const" => ItemKind::Const,
            "static" => ItemKind::Static,
            "type_alias" => ItemKind::TypeAlias,
            "impl" => ItemKind::Impl,
            "trait" => ItemKind::Trait,
            "test" => ItemKind::Test,
            _ => ItemKind::Unknown,
        }
    }
}

// ============================================================================
// Extension Integration
// ============================================================================

/// Find a refactor-capable extension for a file based on its extension.
fn find_refactor_extension(file_path: &str) -> Option<ExtensionManifest> {
    let ext = Path::new(file_path).extension().and_then(|e| e.to_str())?;
    extension::find_extension_for_file_ext(ext, "refactor")
}

/// Try parsing items using the core grammar engine (no extension script needed).
fn core_parse_items(ext: &ExtensionManifest, content: &str) -> Option<Vec<ParsedItem>> {
    let ext_path = ext.extension_path.as_deref()?;
    let file_ext = ext.provided_file_extensions().first()?.clone();
    let grammar = load_extension_grammar(Path::new(ext_path), &file_ext)?;
    let items = grammar_items::parse_items(content, &grammar);
    if items.is_empty() {
        return None;
    }
    Some(items.into_iter().map(ParsedItem::from).collect())
}

/// Ask an extension to parse all top-level items in a source file.
fn ext_parse_items(
    ext: &ExtensionManifest,
    content: &str,
    file_path: &str,
) -> Option<Vec<ParsedItem>> {
    let cmd = serde_json::json!({
        "command": "parse_items",
        "file_path": file_path,
        "content": content,
    });
    let result = extension::run_refactor_script(ext, &cmd)?;
    serde_json::from_value(result.get("items")?.clone()).ok()
}

/// Ask an extension to resolve imports needed in the destination file.
fn ext_resolve_imports(
    ext: &ExtensionManifest,
    moved_items: &[ParsedItem],
    source_content: &str,
    source_path: &str,
    dest_path: &str,
) -> Option<ResolvedImports> {
    let cmd = serde_json::json!({
        "command": "resolve_imports",
        "moved_items": moved_items,
        "source_content": source_content,
        "source_path": source_path,
        "dest_path": dest_path,
    });
    let result = extension::run_refactor_script(ext, &cmd)?;
    serde_json::from_value(result).ok()
}

/// Ask an extension to find test functions related to the moved items.
fn ext_find_related_tests(
    ext: &ExtensionManifest,
    item_names: &[&str],
    content: &str,
    file_path: &str,
) -> Option<RelatedTests> {
    let cmd = serde_json::json!({
        "command": "find_related_tests",
        "item_names": item_names,
        "content": content,
        "file_path": file_path,
    });
    let result = extension::run_refactor_script(ext, &cmd)?;
    serde_json::from_value(result).ok()
}

/// Ask an extension to adjust visibility of items for cross-module use.
fn ext_adjust_visibility(
    ext: &ExtensionManifest,
    items: &[ParsedItem],
    source_path: &str,
    dest_path: &str,
) -> Option<Vec<AdjustedItem>> {
    let cmd = serde_json::json!({
        "command": "adjust_visibility",
        "items": items,
        "source_path": source_path,
        "dest_path": dest_path,
    });
    let result = extension::run_refactor_script(ext, &cmd)?;
    serde_json::from_value(result.get("items")?.clone()).ok()
}

/// Ask an extension to rewrite import paths across the codebase after a move.
/// Returns a list of (file_path, old_line, new_line) replacements.
fn ext_rewrite_caller_imports(
    ext: &ExtensionManifest,
    item_names: &[&str],
    source_module_path: &str,
    dest_module_path: &str,
    file_content: &str,
    file_path: &str,
) -> Option<Vec<ImportRewrite>> {
    let cmd = serde_json::json!({
        "command": "rewrite_caller_imports",
        "item_names": item_names,
        "source_module_path": source_module_path,
        "dest_module_path": dest_module_path,
        "file_content": file_content,
        "file_path": file_path,
    });
    let result = extension::run_refactor_script(ext, &cmd)?;
    serde_json::from_value(result.get("rewrites")?.clone()).ok()
}

/// Ask an extension to generate module index content (mod declarations + pub use re-exports).
///
/// Used after decompose splits a file into submodules — the original file becomes
/// mod.rs and needs `mod submodule;` declarations plus `pub use submodule::*;`
/// re-exports so callers can still find the moved items.
pub fn ext_generate_module_index(
    file_path: &str,
    submodules: &[ModuleIndexEntry],
    remaining_content: &str,
) -> Option<String> {
    let ext = find_refactor_extension(file_path)?;
    let subs: Vec<serde_json::Value> = submodules
        .iter()
        .map(|sub| {
            serde_json::json!({
                "name": sub.name,
                "pub_items": sub.pub_items,
            })
        })
        .collect();
    let cmd = serde_json::json!({
        "command": "generate_module_index",
        "submodules": subs,
        "remaining_content": remaining_content,
    });
    let result = extension::run_refactor_script(&ext, &cmd)?;
    result.get("content")?.as_str().map(|s| s.to_string())
}

/// A submodule entry for module index generation.
#[derive(Debug, Clone)]
pub struct ModuleIndexEntry {
    /// Module name (e.g., "types", "unreleased").
    pub name: String,
    /// Public items that should be re-exported. Empty = glob re-export.
    pub pub_items: Vec<String>,
}

/// A single import rewrite in a caller file.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ImportRewrite {
    /// Line number (1-indexed) in the file.
    pub line: usize,
    /// Original line text.
    pub original: String,
    /// Replacement line text.
    pub replacement: String,
}

// ============================================================================
// Move Operation
// ============================================================================

/// Plan and optionally execute a move of named items from one file to another.
pub fn move_items(
    item_names: &[&str],
    from: &str,
    to: &str,
    root: &Path,
    write: bool,
) -> Result<MoveResult> {
    move_items_with_options(item_names, from, to, root, write, MoveOptions::default())
}

/// Plan and optionally execute a move of named items with custom behavior.
pub fn move_items_with_options(
    item_names: &[&str],
    from: &str,
    to: &str,
    root: &Path,
    write: bool,
    options: MoveOptions,
) -> Result<MoveResult> {
    let from_path = root.join(from);
    let to_path = root.join(to);
    let is_include_fragment = to_path
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext == "inc");

    if !from_path.is_file() {
        return Err(crate::Error::validation_invalid_argument(
            "from",
            format!("Source file does not exist: {}", from),
            None,
            None,
        ));
    }

    let content = std::fs::read_to_string(&from_path)
        .map_err(|e| crate::Error::internal_io(e.to_string(), Some(format!("read {}", from))))?;

    // Try to find a refactor-capable extension for this file type
    let ext = find_refactor_extension(from);
    let mut warnings: Vec<String> = Vec::new();

    // ── Phase 1: Parse items ────────────────────────────────────────────
    // Try core grammar engine first (faster, more robust), fall back to extension script
    let all_items: Vec<ParsedItem> = if let Some(ref ext) = ext {
        core_parse_items(ext, &content).unwrap_or_else(|| {
            ext_parse_items(ext, &content, from).unwrap_or_else(|| {
                warnings.push("Extension parse_items failed, using fallback parser".to_string());
                Vec::new()
            })
        })
    } else {
        warnings.push(
            "No refactor extension found for file type — language-specific features unavailable"
                .to_string(),
        );
        Vec::new()
    };

    if all_items.is_empty() && ext.is_some() {
        // Extension returned nothing — might be a script error
        return Err(crate::Error::validation_invalid_argument(
            "from",
            format!("No items found in {}", from),
            None,
            Some(vec![
                "Check that the file contains parseable top-level items".to_string(),
            ]),
        ));
    } else if all_items.is_empty() {
        return Err(crate::Error::validation_invalid_argument(
            "from",
            format!("No refactor extension available for {} and no items could be parsed", from),
            None,
            Some(vec![
                "Install an extension with refactor capability for this file type".to_string(),
                "Example: homeboy extension install https://github.com/Extra-Chill/homeboy-extensions --id rust".to_string(),
            ]),
        ));
    }

    // Find the requested items
    let mut found_items: Vec<&ParsedItem> = Vec::new();
    let mut missing: Vec<&str> = Vec::new();

    for name in item_names {
        if let Some(item) = all_items.iter().find(|i| i.name == *name) {
            found_items.push(item);
        } else {
            missing.push(name);
        }
    }

    if !missing.is_empty() {
        let available: Vec<&str> = all_items.iter().map(|i| i.name.as_str()).collect();
        return Err(crate::Error::validation_invalid_argument(
            "item",
            format!("Item(s) not found in {}: {}", from, missing.join(", ")),
            None,
            Some(vec![format!("Available items: {}", available.join(", "))]),
        ));
    }

    // ── Phase 2: Find related tests ─────────────────────────────────────
    let related_tests: Vec<ParsedItem> = if options.move_related_tests {
        if let Some(ref ext) = ext {
            ext_find_related_tests(ext, item_names, &content, from)
                .map(|rt| {
                    for name in &rt.ambiguous {
                        warnings.push(format!(
                            "Test '{}' references both moved and unmoved items — skipped",
                            name
                        ));
                    }
                    rt.tests
                })
                .unwrap_or_default()
        } else {
            Vec::new()
        }
    } else {
        Vec::new()
    };

    // ── Phase 3: Adjust visibility ──────────────────────────────────────
    let adjusted_items: Vec<(String, bool)> = if let Some(ref ext) = ext {
        let items_to_adjust: Vec<ParsedItem> = found_items.iter().map(|i| (*i).clone()).collect();
        ext_adjust_visibility(ext, &items_to_adjust, from, to)
            .map(|adjusted| {
                adjusted
                    .into_iter()
                    .map(|a| (a.source, a.changed))
                    .collect()
            })
            .unwrap_or_else(|| {
                found_items
                    .iter()
                    .map(|i| (i.source.clone(), false))
                    .collect()
            })
    } else {
        found_items
            .iter()
            .map(|i| (i.source.clone(), false))
            .collect()
    };

    // ── Phase 4: Resolve imports for destination ────────────────────────
    let dest_imports: Vec<String> = if is_include_fragment {
        Vec::new()
    } else if let Some(ref ext) = ext {
        let items_for_resolve: Vec<ParsedItem> = found_items.iter().map(|i| (*i).clone()).collect();
        ext_resolve_imports(ext, &items_for_resolve, &content, from, to)
            .map(|ri| {
                for w in &ri.warnings {
                    warnings.push(w.clone());
                }
                ri.needed_imports
            })
            .unwrap_or_default()
    } else {
        Vec::new()
    };

    // ── Phase 5: Build destination content ──────────────────────────────
    let dest_exists = to_path.is_file();
    let existing_dest = if dest_exists {
        std::fs::read_to_string(&to_path).unwrap_or_default()
    } else {
        String::new()
    };

    let mut dest_additions = String::new();
    if !dest_exists {
        if !is_include_fragment {
            // New file — add module doc comment and imports
            let module_name = to_path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("module");
            let from_basename = Path::new(from)
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or(from);
            dest_additions.push_str(&format!(
                "//! {} — extracted from {}.\n\n",
                module_name, from_basename
            ));

            // Add resolved imports
            for imp in &dest_imports {
                dest_additions.push_str(imp);
                if !imp.ends_with('\n') {
                    dest_additions.push('\n');
                }
            }
            if !dest_imports.is_empty() {
                dest_additions.push('\n');
            }
            dest_additions.push('\n');
        }
    } else {
        // Existing file — add imports that aren't already present
        let new_imports: Vec<&String> = dest_imports
            .iter()
            .filter(|imp| !existing_dest.contains(imp.trim()))
            .collect();
        if !new_imports.is_empty() {
            // Find the last import line in the existing file to insert after
            dest_additions.push('\n');
            for imp in &new_imports {
                dest_additions.push_str(imp);
                if !imp.ends_with('\n') {
                    dest_additions.push('\n');
                }
            }
        }
        dest_additions.push('\n');
    }

    // Add the items (in original source order), using visibility-adjusted source
    let mut items_in_order: Vec<(usize, &ParsedItem, &str)> = found_items
        .iter()
        .enumerate()
        .map(|(idx, item)| (item.start_line, *item, adjusted_items[idx].0.as_str()))
        .collect();
    items_in_order.sort_by_key(|(line, _, _)| *line);

    for (idx, (_, _, adjusted_source)) in items_in_order.iter().enumerate() {
        if idx > 0 {
            dest_additions.push('\n');
        }
        dest_additions.push_str(adjusted_source);
        dest_additions.push('\n');
    }

    // Add related tests if any
    if !related_tests.is_empty() {
        dest_additions.push_str("\n#[cfg(test)]\nmod tests {\n    use super::*;\n\n");
        for (idx, test) in related_tests.iter().enumerate() {
            if idx > 0 {
                dest_additions.push('\n');
            }
            // Indent each line of the test by 4 spaces
            for line in test.source.lines() {
                if line.is_empty() {
                    dest_additions.push('\n');
                } else {
                    dest_additions.push_str("    ");
                    dest_additions.push_str(line);
                    dest_additions.push('\n');
                }
            }
        }
        dest_additions.push_str("}\n");
    }

    // ── Phase 6: Build modified source (remove items + tests) ───────────
    let lines: Vec<&str> = content.lines().collect();
    let mut source_lines_keep: Vec<bool> = vec![true; lines.len()];

    // Remove moved items (descending order to not shift indices)
    let mut items_to_remove: Vec<&ParsedItem> = found_items.clone();
    items_to_remove.extend(related_tests.iter());
    items_to_remove.sort_by(|a, b| b.start_line.cmp(&a.start_line));

    for item in &items_to_remove {
        let start = item.start_line.saturating_sub(1); // 0-indexed
        let end = item.end_line.saturating_sub(1); // 0-indexed

        // Also remove any blank line immediately after the item (cosmetic)
        let actual_end = if end + 1 < lines.len() && lines[end + 1].trim().is_empty() {
            end + 1
        } else {
            end
        };

        for j in start..=actual_end {
            if j < source_lines_keep.len() {
                source_lines_keep[j] = false;
            }
        }
    }

    let modified_source: String = lines
        .iter()
        .enumerate()
        .filter(|(i, _)| source_lines_keep[*i])
        .map(|(_, l)| *l)
        .collect::<Vec<_>>()
        .join("\n");
    // Preserve trailing newline
    let modified_source = if content.ends_with('\n') && !modified_source.ends_with('\n') {
        modified_source + "\n"
    } else {
        modified_source
    };

    let final_dest = if dest_exists {
        format!("{}{}", existing_dest.trim_end(), dest_additions)
    } else {
        dest_additions
    };

    // ── Phase 7: Update caller imports across the codebase ──────────────
    let mut imports_updated: usize = 0;
    let mut caller_rewrites: Vec<(PathBuf, Vec<ImportRewrite>)> = Vec::new();

    if let Some(ref ext) = ext {
        // Walk source files to find callers that import the moved items
        let source_module = module_path_from_file(from);
        let dest_module = module_path_from_file(to);

        if source_module != dest_module {
            let all_files = codebase_scan::walk_files(
                root,
                &ScanConfig {
                    extensions: ExtensionFilter::All,
                    ..Default::default()
                },
            );
            for file_path in &all_files {
                let rel_path = file_path
                    .strip_prefix(root)
                    .unwrap_or(file_path)
                    .to_string_lossy()
                    .to_string();

                // Skip source and destination files (we handle those directly)
                if rel_path == from || rel_path == to {
                    continue;
                }

                // Only check files the extension can handle
                let file_ext = file_path.extension().and_then(|e| e.to_str()).unwrap_or("");
                if !ext.handles_file_extension(file_ext) {
                    continue;
                }

                let file_content = match std::fs::read_to_string(file_path) {
                    Ok(c) => c,
                    Err(_) => continue,
                };

                // Quick check: does this file mention any of the moved items?
                let mentions_moved = item_names.iter().any(|name| file_content.contains(name));
                if !mentions_moved {
                    continue;
                }

                if let Some(rewrites) = ext_rewrite_caller_imports(
                    ext,
                    item_names,
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

    // ── Phase 8: Build result ───────────────────────────────────────────
    let items_moved: Vec<MovedItem> = found_items
        .iter()
        .map(|item| MovedItem {
            name: item.name.clone(),
            kind: ItemKind::from_str(&item.kind),
            source_lines: (item.start_line, item.end_line),
            line_count: item.end_line - item.start_line + 1,
        })
        .collect();

    let tests_moved: Vec<MovedItem> = related_tests
        .iter()
        .map(|item| MovedItem {
            name: item.name.clone(),
            kind: ItemKind::Test,
            source_lines: (item.start_line, item.end_line),
            line_count: item.end_line - item.start_line + 1,
        })
        .collect();

    let file_created = !dest_exists;

    // ── Phase 9: Apply if requested ─────────────────────────────────────
    if write {
        // Create parent directory if needed
        if let Some(parent) = to_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                crate::Error::internal_io(e.to_string(), Some(format!("create dir for {}", to)))
            })?;
        }

        // Write destination
        std::fs::write(&to_path, &final_dest)
            .map_err(|e| crate::Error::internal_io(e.to_string(), Some(format!("write {}", to))))?;

        // Write modified source
        std::fs::write(&from_path, &modified_source).map_err(|e| {
            crate::Error::internal_io(e.to_string(), Some(format!("write {}", from)))
        })?;

        // Apply caller import rewrites
        for (file_path, rewrites) in &caller_rewrites {
            let file_content = std::fs::read_to_string(file_path).map_err(|e| {
                crate::Error::internal_io(
                    e.to_string(),
                    Some(format!("read {}", file_path.display())),
                )
            })?;
            let mut file_lines: Vec<String> = file_content.lines().map(String::from).collect();

            for rewrite in rewrites {
                let idx = rewrite.line.saturating_sub(1);
                if idx < file_lines.len() {
                    file_lines[idx] = rewrite.replacement.clone();
                }
            }

            let modified = file_lines.join("\n");
            let modified = if file_content.ends_with('\n') && !modified.ends_with('\n') {
                modified + "\n"
            } else {
                modified
            };

            std::fs::write(file_path, &modified).map_err(|e| {
                crate::Error::internal_io(
                    e.to_string(),
                    Some(format!("write {}", file_path.display())),
                )
            })?;
        }

        crate::log_status!(
            "refactor",
            "Moved {} item(s) from {} to {}",
            items_moved.len(),
            from,
            to
        );
        if !tests_moved.is_empty() {
            crate::log_status!("refactor", "Moved {} related test(s)", tests_moved.len());
        }
        if imports_updated > 0 {
            crate::log_status!(
                "refactor",
                "Updated {} import(s) across {} file(s)",
                imports_updated,
                caller_rewrites.len()
            );
        }
    }

    Ok(MoveResult {
        items_moved,
        from_file: from.to_string(),
        to_file: to.to_string(),
        file_created,
        imports_updated,
        caller_files_modified: caller_rewrites
            .iter()
            .map(|(path, _)| path.clone())
            .collect(),
        tests_moved,
        applied: write,
        warnings,
    })
}

// ============================================================================
// Whole-file move
// ============================================================================

/// Result of a whole-file move operation.
#[derive(Debug, Clone, serde::Serialize)]
pub struct MoveFileResult {
    /// The source file that was moved.
    pub from_file: String,
    /// The destination file.
    pub to_file: String,
    /// Number of import references updated across the codebase.
    pub imports_updated: usize,
    /// Files whose imports were rewritten.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub caller_files_modified: Vec<String>,
    /// Whether changes were written to disk.
    pub applied: bool,
    /// Warnings generated during the move.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    /// Whether mod declarations were updated.
    pub mod_declarations_updated: bool,
}

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
    let mut warnings = Vec::new();

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
            let source_module_leaf = source_module
                .rsplit("::")
                .next()
                .unwrap_or(&source_module);

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

                let file_ext_str =
                    file_path.extension().and_then(|e| e.to_str()).unwrap_or("");
                if !ext.handles_file_extension(file_ext_str) {
                    continue;
                }

                let file_content = match std::fs::read_to_string(file_path) {
                    Ok(c) => c,
                    Err(_) => continue,
                };

                // Quick check: does this file mention the source module?
                let mentions_source =
                    search_refs.iter().any(|term| file_content.contains(term));
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
            crate::Error::internal_io(
                e.to_string(),
                Some(format!("move {} → {}", from, to)),
            )
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
fn find_parent_mod_rs(file_path: &str, root: &Path) -> Option<PathBuf> {
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
fn remove_mod_declaration(content: &str, module_name: &str) -> String {
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
fn add_mod_declaration(content: &str, module_name: &str) -> String {
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
        if trimmed.starts_with("mod ") || trimmed.starts_with("pub mod ")
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

// ============================================================================
// Helpers
// ============================================================================

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

/// Walk source files recursively, skipping common non-source directories.
fn walk_source_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    walk_recursive(root, root, &mut files);
    files
}

/// Directories to always skip at any depth.
const ALWAYS_SKIP_DIRS: &[&str] = &["node_modules", "vendor", ".git", ".svn", ".hg"];

/// Directories to skip only at root level.
const ROOT_ONLY_SKIP_DIRS: &[&str] = &["build", "dist", "target", "cache", "tmp"];

fn walk_recursive(dir: &Path, root: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };

    let is_root = dir == root;

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            if ALWAYS_SKIP_DIRS.contains(&name.as_str()) {
                continue;
            }
            if is_root && ROOT_ONLY_SKIP_DIRS.contains(&name.as_str()) {
                continue;
            }
            walk_recursive(&path, root, files);
        } else if path.is_file() {
            files.push(path);
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn module_path_from_file_basic() {
        assert_eq!(
            module_path_from_file("src/core/code_audit/conventions.rs"),
            "core::code_audit::conventions"
        );
    }

    #[test]
    fn module_path_from_file_mod() {
        assert_eq!(
            module_path_from_file("src/core/code_audit/mod.rs"),
            "core::code_audit"
        );
    }

    #[test]
    fn module_path_from_file_no_src_prefix() {
        assert_eq!(module_path_from_file("lib/utils.rs"), "lib::utils");
    }

    #[test]
    fn item_kind_from_str() {
        assert!(matches!(ItemKind::from_str("function"), ItemKind::Function));
        assert!(matches!(ItemKind::from_str("struct"), ItemKind::Struct));
        assert!(matches!(ItemKind::from_str("test"), ItemKind::Test));
        assert!(matches!(ItemKind::from_str("bogus"), ItemKind::Unknown));
    }
}
