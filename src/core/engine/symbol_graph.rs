//! Symbol reference graph — trace imports, callers, and dependencies across a codebase.
//!
//! Core primitive for understanding how symbols connect across files.
//! Used by: fixer (caller rewriting after dedup), move_items (import rewriting),
//! impact tracing (changed-since), dead code detection.
//!
//! # Architecture
//!
//! ```text
//! utils/grammar.rs           (structural parsing, import pattern matching)
//!     ↓
//! core/symbol_graph.rs       (this file: reference graph, import rewriting)
//!     ↓
//! consumers:                 fixer, move_items, impact, dead_code
//! ```
//!
//! Grammar-driven — no extension subprocesses needed. As long as a language
//! has an `import` pattern in its grammar.toml, the full graph works.

use std::collections::HashMap;
use std::path::Path;

use crate::engine::codebase_scan::{self, ExtensionFilter, ScanConfig};
use crate::extension::grammar::{self, Grammar};

// ============================================================================
// Types
// ============================================================================

/// A parsed import statement in a source file.
#[derive(Debug, Clone)]
pub struct ImportRef {
    /// Relative file path from project root.
    pub file: String,
    /// 1-indexed line number of the import statement.
    pub line: usize,
    /// The raw module path from the import (e.g., `crate::core::fixer`).
    pub module_path: String,
    /// Individual symbol names imported (e.g., `["module_path_from_file", "insertion"]`).
    /// Empty if the import is a wildcard or module-level.
    pub imported_names: Vec<String>,
    /// The full original line text.
    pub original_text: String,
}

/// A file that references a symbol, with details about how.
#[derive(Debug, Clone)]
pub struct CallerRef {
    /// Relative file path that contains the reference.
    pub file: String,
    /// The import statement that brings the symbol in (if any).
    pub import: Option<ImportRef>,
    /// Whether the file also calls the symbol (found in content).
    pub has_call_site: bool,
}

/// Result of rewriting an import line.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ImportRewrite {
    /// Relative file path.
    pub file: String,
    /// 1-indexed line number.
    pub line: usize,
    /// Original line text.
    pub original: String,
    /// Replacement line text.
    pub replacement: String,
}

/// Result of a symbol rewrite operation.
#[derive(Debug, Clone)]
pub struct RewriteResult {
    /// Files that had imports rewritten.
    pub rewrites: Vec<ImportRewrite>,
    /// Files where the symbol is called but no import was found to rewrite.
    /// These may need manual attention.
    pub unresolved_callers: Vec<String>,
    /// Whether changes were written to disk.
    pub applied: bool,
}

// ============================================================================
// Module path utilities
// ============================================================================

/// Convert a file path to a Rust module path.
///
/// `"src/core/code_audit/conventions.rs"` → `"core::code_audit::conventions"`
/// `"src/core/code_audit/mod.rs"` → `"core::code_audit"`
/// `"lib/utils.rs"` → `"lib::utils"`
pub fn module_path_from_file(file_path: &str) -> String {
    let p = file_path.strip_prefix("src/").unwrap_or(file_path);
    let p = p.strip_suffix(".rs").unwrap_or(p);
    let p = p.strip_suffix("/mod").unwrap_or(p);
    p.replace('/', "::")
}

// ============================================================================
// Import parsing — grammar-driven
// ============================================================================

/// Parse all import statements from a source file using its grammar.
///
/// Returns structured `ImportRef`s with module paths and imported names.
/// For Rust, handles both simple (`use crate::mod::Item;`) and grouped
/// (`use crate::mod::{A, B};`) imports.
pub(crate) fn parse_imports(content: &str, grammar: &Grammar, relative_path: &str) -> Vec<ImportRef> {
    let symbols = grammar::extract(content, grammar);
    let lines: Vec<&str> = content.lines().collect();
    let language_id = grammar.language.id.as_str();

    symbols
        .iter()
        .filter(|s| s.concept == "import")
        .filter_map(|s| {
            let raw_path = s.get("path")?;
            let line_text = lines.get(s.line.saturating_sub(1)).unwrap_or(&"");

            let (module_path, imported_names) = match language_id {
                "rust" => parse_rust_import_path(raw_path),
                "php" | "wordpress" => parse_php_import_path(raw_path),
                _ => (raw_path.to_string(), vec![]),
            };

            Some(ImportRef {
                file: relative_path.to_string(),
                line: s.line,
                module_path,
                imported_names,
                original_text: line_text.to_string(),
            })
        })
        .collect()
}

/// Parse a Rust `use` path into module path + imported names.
///
/// `"crate::core::fixer::module_path_from_file"` → (`"crate::core::fixer"`, `["module_path_from_file"]`)
/// `"crate::core::fixer::{insertion, Fix}"` → (`"crate::core::fixer"`, `["insertion", "Fix"]`)
/// `"std::path::Path"` → (`"std::path"`, `["Path"]`)
fn parse_rust_import_path(raw: &str) -> (String, Vec<String>) {
    // Handle grouped imports: crate::mod::{A, B}
    if let Some(brace_start) = raw.find("::{") {
        let module = &raw[..brace_start];
        let inner = raw[brace_start + 3..]
            .trim_end_matches('}')
            .split(',')
            .map(|s| {
                let s = s.trim();
                // Handle `Name as Alias` — use the original name
                if let Some(pos) = s.find(" as ") {
                    s[..pos].trim().to_string()
                } else {
                    s.to_string()
                }
            })
            .filter(|s| !s.is_empty() && s != "self")
            .collect();
        (module.to_string(), inner)
    } else {
        // Simple import: the last segment is the imported name
        if let Some(last_sep) = raw.rfind("::") {
            let module = &raw[..last_sep];
            let name = &raw[last_sep + 2..];
            if name == "self" || name == "*" {
                (raw.to_string(), vec![])
            } else {
                (module.to_string(), vec![name.to_string()])
            }
        } else {
            // No :: at all — top-level import
            (raw.to_string(), vec![])
        }
    }
}

/// Parse a PHP `use` path into module path + imported name.
///
/// `"App\\Models\\User"` → (`"App\\Models"`, `["User"]`)
fn parse_php_import_path(raw: &str) -> (String, Vec<String>) {
    if let Some(last_sep) = raw.rfind('\\') {
        let module = &raw[..last_sep];
        let name = &raw[last_sep + 1..];
        (module.to_string(), vec![name.to_string()])
    } else {
        (raw.to_string(), vec![])
    }
}

// ============================================================================
// Symbol tracing
// ============================================================================

/// Find all files that reference a symbol exported from a given module.
///
/// Walks the codebase, parses imports in each file, and checks:
/// 1. Does the file import `symbol_name` from `source_module`?
/// 2. Does the file content mention `symbol_name`? (call site check)
///
/// Returns `CallerRef` for each file that references the symbol.
pub fn trace_symbol_callers(
    symbol_name: &str,
    source_module: &str,
    root: &Path,
    file_extensions: &[&str],
) -> Vec<CallerRef> {
    let config = ScanConfig {
        extensions: ExtensionFilter::Only(file_extensions.iter().map(|e| e.to_string()).collect()),
        skip_hidden: true,
        ..Default::default()
    };

    let files = codebase_scan::walk_files(root, &config);
    let mut callers = Vec::new();

    for file_path in &files {
        let rel_path = file_path
            .strip_prefix(root)
            .unwrap_or(file_path)
            .to_string_lossy()
            .to_string();

        let content = match std::fs::read_to_string(file_path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        // Quick pre-filter: does the file mention the symbol at all?
        if !content.contains(symbol_name) {
            continue;
        }

        let ext = file_path.extension().and_then(|e| e.to_str()).unwrap_or("");

        // Parse imports using grammar
        let matching_import = if let Some(grammar) = load_grammar_for_ext(ext) {
            let imports = parse_imports(&content, &grammar, &rel_path);
            imports.into_iter().find(|imp| {
                imp.imported_names.contains(&symbol_name.to_string())
                    && import_matches_module(&imp.module_path, source_module)
            })
        } else {
            None
        };

        let has_import = matching_import.is_some();
        let has_call_site = content.contains(&format!("{}(", symbol_name))
            || content.contains(&format!("{}::", symbol_name))
            || content.contains(&format!(".{}", symbol_name));

        if has_import || has_call_site {
            callers.push(CallerRef {
                file: rel_path,
                import: matching_import,
                has_call_site,
            });
        }
    }

    callers
}

/// Check if an import's module path matches a source module.
///
/// Handles both absolute (`crate::core::fixer`) and the import's
/// own module path (`core::fixer` without crate:: prefix).
fn import_matches_module(import_module: &str, source_module: &str) -> bool {
    // Direct match
    if import_module == source_module {
        return true;
    }
    // Match with crate:: prefix
    let with_crate = format!("crate::{}", source_module);
    if import_module == with_crate {
        return true;
    }
    // Match without crate:: prefix
    let without_crate = source_module
        .strip_prefix("crate::")
        .unwrap_or(source_module);
    if import_module == without_crate {
        return true;
    }
    // Import has crate:: but source doesn't
    let import_without = import_module
        .strip_prefix("crate::")
        .unwrap_or(import_module);
    import_without == source_module || import_without == without_crate
}

// ============================================================================
// Import rewriting
// ============================================================================

/// Rewrite all imports of a symbol from one module to another.
///
/// Walks the codebase, finds files that import `symbol_name` from
/// `old_module`, and rewrites those imports to point to `new_module`.
///
/// If `write` is true, applies changes to disk.
pub fn rewrite_imports(
    symbol_name: &str,
    old_module: &str,
    new_module: &str,
    root: &Path,
    file_extensions: &[&str],
    write: bool,
) -> RewriteResult {
    let callers = trace_symbol_callers(symbol_name, old_module, root, file_extensions);
    let mut rewrites = Vec::new();
    let mut unresolved = Vec::new();

    for caller in &callers {
        if let Some(ref import) = caller.import {
            if let Some(rewrite) = compute_import_rewrite(import, symbol_name, new_module) {
                rewrites.push(rewrite);
            }
        } else if caller.has_call_site {
            // File calls the symbol but has no matching import — may use
            // a wildcard import, re-export, or local definition.
            unresolved.push(caller.file.clone());
        }
    }

    if write {
        apply_rewrites(&rewrites, root);
    }

    RewriteResult {
        rewrites,
        unresolved_callers: unresolved,
        applied: write,
    }
}

/// Compute the rewritten import line for a single file.
fn compute_import_rewrite(
    import: &ImportRef,
    symbol_name: &str,
    new_module: &str,
) -> Option<ImportRewrite> {
    let original = &import.original_text;
    // Determine the indentation
    let indent = &original[..original.len() - original.trim_start().len()];

    if import.imported_names.len() == 1 {
        // Simple import — rewrite the whole line
        let new_module_with_crate = if new_module.starts_with("crate::") {
            new_module.to_string()
        } else {
            format!("crate::{}", new_module)
        };
        let replacement = format!("{}use {}::{};", indent, new_module_with_crate, symbol_name);
        Some(ImportRewrite {
            file: import.file.clone(),
            line: import.line,
            original: original.to_string(),
            replacement,
        })
    } else if import.imported_names.len() > 1 {
        // Grouped import — need to remove the symbol from the group and add a new import
        // For now, rewrite the whole line to split out the symbol.
        // This handles: `use crate::mod::{A, B, symbol};` → remove symbol from group + add new import.

        // Build the group without the moved symbol
        let remaining: Vec<&String> = import
            .imported_names
            .iter()
            .filter(|n| n.as_str() != symbol_name)
            .collect();

        if remaining.is_empty() {
            // All names were just this symbol — replace the whole line
            let new_module_with_crate = if new_module.starts_with("crate::") {
                new_module.to_string()
            } else {
                format!("crate::{}", new_module)
            };
            let replacement = format!("{}use {}::{};", indent, new_module_with_crate, symbol_name);
            Some(ImportRewrite {
                file: import.file.clone(),
                line: import.line,
                original: original.to_string(),
                replacement,
            })
        } else {
            // Keep remaining in the original group, add new import on a new line
            let old_module_with_crate = if import.module_path.starts_with("crate::") {
                import.module_path.clone()
            } else {
                format!("crate::{}", import.module_path)
            };
            let new_module_with_crate = if new_module.starts_with("crate::") {
                new_module.to_string()
            } else {
                format!("crate::{}", new_module)
            };

            let remaining_str = if remaining.len() == 1 {
                format!("{}use {}::{};", indent, old_module_with_crate, remaining[0])
            } else {
                format!(
                    "{}use {}::{{{}}};",
                    indent,
                    old_module_with_crate,
                    remaining
                        .iter()
                        .map(|s| s.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            };

            let replacement = format!(
                "{}\n{}use {}::{};",
                remaining_str, indent, new_module_with_crate, symbol_name
            );

            Some(ImportRewrite {
                file: import.file.clone(),
                line: import.line,
                original: original.to_string(),
                replacement,
            })
        }
    } else {
        // Module-level or wildcard import — can't determine which symbols
        None
    }
}

/// Apply import rewrites to disk.
fn apply_rewrites(rewrites: &[ImportRewrite], root: &Path) {
    // Group rewrites by file
    let mut by_file: HashMap<&str, Vec<&ImportRewrite>> = HashMap::new();
    for rewrite in rewrites {
        by_file
            .entry(rewrite.file.as_str())
            .or_default()
            .push(rewrite);
    }

    for (file, file_rewrites) in &by_file {
        let abs_path = root.join(file);
        let Ok(content) = std::fs::read_to_string(&abs_path) else {
            continue;
        };

        let mut lines: Vec<String> = content.lines().map(String::from).collect();

        // Apply rewrites in reverse line order to avoid index shifting
        let mut sorted_rewrites: Vec<&&ImportRewrite> = file_rewrites.iter().collect();
        sorted_rewrites.sort_by(|a, b| b.line.cmp(&a.line));

        for rewrite in sorted_rewrites {
            let idx = rewrite.line.saturating_sub(1);
            if idx < lines.len() {
                // The replacement may contain newlines (for grouped import splits)
                let replacement_lines: Vec<&str> = rewrite.replacement.lines().collect();
                lines.splice(idx..=idx, replacement_lines.iter().map(|s| s.to_string()));
            }
        }

        let mut modified = lines.join("\n");
        if content.ends_with('\n') && !modified.ends_with('\n') {
            modified.push('\n');
        }

        let _ = std::fs::write(&abs_path, &modified);
    }
}

// ============================================================================
// Grammar loading (shared with core_fingerprint)
// ============================================================================

/// Load a grammar for a file extension.
///
/// This is a shared utility — same as `core_fingerprint::load_grammar_for_ext`
/// but accessible from the symbol_graph module without circular dependency.
fn load_grammar_for_ext(ext: &str) -> Option<Grammar> {
    let matched = crate::extension::find_extension_for_file_ext(ext, "fingerprint")?;
    let extension_path = matched.extension_path.as_deref()?;

    let grammar_path = Path::new(extension_path).join("grammar.toml");
    if grammar_path.exists() {
        return grammar::load_grammar(&grammar_path).ok();
    }

    let grammar_json_path = Path::new(extension_path).join("grammar.json");
    if grammar_json_path.exists() {
        return grammar::load_grammar_json(&grammar_json_path).ok();
    }

    None
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn module_path_from_file_strips_src_and_extension() {
        assert_eq!(
            module_path_from_file("src/core/code_audit/conventions.rs"),
            "core::code_audit::conventions"
        );
    }

    #[test]
    fn module_path_from_file_handles_mod_rs() {
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
    fn import_matches_module_variants() {
        // Direct match
        assert!(import_matches_module("core::fixer", "core::fixer"));
        // With crate prefix
        assert!(import_matches_module("crate::core::fixer", "core::fixer"));
        // Source has crate prefix
        assert!(import_matches_module("core::fixer", "crate::core::fixer"));
        // Both have crate prefix
        assert!(import_matches_module(
            "crate::core::fixer",
            "crate::core::fixer"
        ));
        // No match
        assert!(!import_matches_module("core::fixer", "core::other"));
    }

    #[test]
    fn parse_imports_with_rust_grammar() {
        let grammar_path =
            std::path::Path::new("/root/.config/homeboy/extensions/rust/grammar.toml");
        if !grammar_path.exists() {
            return; // Skip if grammar not installed
        }
        let grammar = crate::extension::grammar::load_grammar(grammar_path).unwrap();

        let content = r#"use std::path::Path;
use crate::core::fixer::{insertion, Fix};
use crate::extension::grammar;

pub fn hello() {}
"#;

        let imports = parse_imports(content, &grammar, "src/example.rs");

        assert_eq!(imports.len(), 3);

        // First import: std::path::Path
        assert_eq!(imports[0].module_path, "std::path");
        assert_eq!(imports[0].imported_names, vec!["Path"]);
        assert_eq!(imports[0].line, 1);

        // Second import: grouped
        assert_eq!(imports[1].module_path, "crate::core::fixer");
        assert_eq!(imports[1].imported_names, vec!["insertion", "Fix"]);

        // Third import: module-level use
        assert_eq!(imports[2].module_path, "crate::extension");
        assert_eq!(imports[2].imported_names, vec!["grammar"]);
    }

}
