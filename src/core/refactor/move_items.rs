//! Refactor move — extract items from one file and move them to another.
//!
//! Identifies named items (functions, structs, enums, consts, impl blocks, type aliases)
//! by parsing Rust source, extracts them including doc comments and attributes,
//! and writes them to a destination file. Updates imports across the codebase.
//!
//! Usage:
//!   `homeboy refactor move --item "has_import" --from src/code_audit/conventions.rs --to src/code_audit/import_matching.rs`

use std::path::{Path, PathBuf};

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
    /// Whether changes were written to disk.
    pub applied: bool,
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
}

/// A located item in a source file — its boundaries and metadata.
#[derive(Debug, Clone)]
struct LocatedItem {
    /// Name of the item.
    name: String,
    /// Kind of item.
    kind: ItemKind,
    /// Start line (1-indexed, includes doc comments and attributes).
    start_line: usize,
    /// End line (1-indexed, inclusive).
    end_line: usize,
    /// The extracted source code (including doc comments and attributes).
    source: String,
    /// Visibility (pub, pub(crate), pub(super), or empty for private).
    #[allow(dead_code)]
    visibility: String,
}

// ============================================================================
// Item Location
// ============================================================================

/// Find all top-level items in a Rust source file.
fn locate_items(content: &str) -> Vec<LocatedItem> {
    let lines: Vec<&str> = content.lines().collect();
    let mut items = Vec::new();
    let mut i = 0;

    while i < lines.len() {
        // Try to parse an item starting at this line (or a doc/attr prefix leading to one)
        if let Some(item) = try_locate_item(&lines, i) {
            i = item.end_line; // Skip past this item
            items.push(item);
        } else {
            i += 1;
        }
    }

    items
}

/// Try to locate an item starting at line index `start` (0-indexed).
/// Collects doc comments and attributes above the item declaration.
fn try_locate_item(lines: &[&str], start: usize) -> Option<LocatedItem> {
    let trimmed = lines[start].trim();

    // Skip blank lines, use statements, and mod declarations
    if trimmed.is_empty()
        || trimmed.starts_with("use ")
        || trimmed.starts_with("mod ")
        || trimmed.starts_with("//!")
    {
        return None;
    }

    // Check if this line starts a doc comment / attribute block or an item directly
    let (prefix_start, decl_line_idx) = if trimmed.starts_with("///")
        || trimmed.starts_with("#[")
    {
        // Scan forward through doc comments and attributes to find the declaration
        let mut j = start;
        while j < lines.len() {
            let t = lines[j].trim();
            if t.starts_with("///") || t.starts_with("#[") || t.is_empty() {
                j += 1;
            } else {
                break;
            }
        }
        if j >= lines.len() {
            return None;
        }
        (start, j)
    } else {
        (start, start)
    };

    let decl = lines[decl_line_idx].trim();

    // Parse the declaration to determine item kind and name
    let (kind, name, visibility) = parse_item_declaration(decl)?;

    // Find the end of the item
    let end_line_idx = find_item_end(lines, decl_line_idx, &kind);

    // Extract the source
    let source_lines: Vec<&str> = lines[prefix_start..=end_line_idx].to_vec();
    let source = source_lines.join("\n");

    Some(LocatedItem {
        name,
        kind,
        start_line: prefix_start + 1, // 1-indexed
        end_line: end_line_idx + 1,     // 1-indexed
        source,
        visibility,
    })
}

/// Parse an item declaration line to extract kind, name, and visibility.
fn parse_item_declaration(decl: &str) -> Option<(ItemKind, String, String)> {
    // Extract visibility prefix
    let (vis, rest) = extract_visibility(decl);

    // Match against known patterns
    if let Some(name) = extract_after_keyword(rest, "fn ") {
        // Function — extract name before '(' or '<'
        let name = name.split(['(', '<']).next()?.trim().to_string();
        Some((ItemKind::Function, name, vis))
    } else if let Some(name) = extract_after_keyword(rest, "struct ") {
        let name = name.split(['{', '(', '<', ';']).next()?.trim().to_string();
        Some((ItemKind::Struct, name, vis))
    } else if let Some(name) = extract_after_keyword(rest, "enum ") {
        let name = name.split(['{', '<']).next()?.trim().to_string();
        Some((ItemKind::Enum, name, vis))
    } else if let Some(name) = extract_after_keyword(rest, "const ") {
        let name = name.split([':','=']).next()?.trim().to_string();
        Some((ItemKind::Const, name, vis))
    } else if let Some(name) = extract_after_keyword(rest, "static ") {
        let name = name.split([':','=']).next()?.trim().to_string();
        Some((ItemKind::Static, name, vis))
    } else if let Some(name) = extract_after_keyword(rest, "type ") {
        let name = name.split(['=', '<']).next()?.trim().to_string();
        Some((ItemKind::TypeAlias, name, vis))
    } else if let Some(name) = extract_after_keyword(rest, "trait ") {
        let name = name.split(['{', '<', ':']).next()?.trim().to_string();
        Some((ItemKind::Trait, name, vis))
    } else if rest.starts_with("impl") {
        // impl blocks: `impl Foo { ... }` or `impl Foo for Bar { ... }`
        let after_impl = rest.strip_prefix("impl")?.trim();
        let name = after_impl.split(['{', '<']).next()?.trim().to_string();
        Some((ItemKind::Impl, name, vis))
    } else {
        None
    }
}

/// Extract visibility prefix from a declaration.
fn extract_visibility(decl: &str) -> (String, &str) {
    if let Some(rest) = decl.strip_prefix("pub(crate) ") {
        ("pub(crate)".to_string(), rest)
    } else if let Some(rest) = decl.strip_prefix("pub(super) ") {
        ("pub(super)".to_string(), rest)
    } else if let Some(rest) = decl.strip_prefix("pub ") {
        ("pub".to_string(), rest)
    } else {
        (String::new(), decl)
    }
}

/// Extract text after a keyword (e.g., "fn " -> rest after "fn ").
fn extract_after_keyword<'a>(text: &'a str, keyword: &str) -> Option<&'a str> {
    // Handle `async fn`, `unsafe fn`, etc.
    let search = text;
    if let Some(idx) = search.find(keyword) {
        Some(&search[idx + keyword.len()..])
    } else {
        None
    }
}

/// Find the end line of an item by matching braces.
/// For items without braces (const, static, type alias), finds the semicolon.
fn find_item_end(lines: &[&str], decl_line: usize, kind: &ItemKind) -> usize {
    match kind {
        ItemKind::Const | ItemKind::Static | ItemKind::TypeAlias => {
            // These end at a semicolon
            for i in decl_line..lines.len() {
                if lines[i].contains(';') {
                    return i;
                }
            }
            decl_line
        }
        ItemKind::Struct => {
            // Could be a tuple struct (ends with ;) or a braced struct
            let combined: String = lines[decl_line..].iter()
                .take(3)
                .copied()
                .collect::<Vec<_>>()
                .join(" ");
            if combined.contains(';') && !combined.contains('{') {
                // Tuple struct or unit struct
                for i in decl_line..lines.len() {
                    if lines[i].contains(';') {
                        return i;
                    }
                }
                return decl_line;
            }
            // Braced struct — fall through to brace matching
            find_matching_brace(lines, decl_line)
        }
        _ => {
            // Function, enum, impl, trait — all end with matched braces
            find_matching_brace(lines, decl_line)
        }
    }
}

/// Find the line index where braces balance back to zero, starting from `start_line`.
///
/// Skips braces inside string literals, character literals, and comments to avoid
/// false matches from content like `"serde::{Deserialize, Serialize}"`.
fn find_matching_brace(lines: &[&str], start_line: usize) -> usize {
    let mut depth: i32 = 0;
    let mut found_open = false;
    let mut in_block_comment = false;

    for i in start_line..lines.len() {
        let line = lines[i];
        let chars: Vec<char> = line.chars().collect();
        let mut j = 0;

        // Check for line comments (skip rest of line)
        while j < chars.len() {
            if in_block_comment {
                if j + 1 < chars.len() && chars[j] == '*' && chars[j + 1] == '/' {
                    in_block_comment = false;
                    j += 2;
                } else {
                    j += 1;
                }
                continue;
            }

            // Start of block comment
            if j + 1 < chars.len() && chars[j] == '/' && chars[j + 1] == '*' {
                in_block_comment = true;
                j += 2;
                continue;
            }

            // Line comment — skip rest of line
            if j + 1 < chars.len() && chars[j] == '/' && chars[j + 1] == '/' {
                break;
            }

            // String literal — skip to closing quote
            if chars[j] == '"' {
                j += 1;
                while j < chars.len() {
                    if chars[j] == '\\' {
                        j += 2; // Skip escaped character
                    } else if chars[j] == '"' {
                        j += 1;
                        break;
                    } else {
                        j += 1;
                    }
                }
                continue;
            }

            // Character literal — skip to closing quote
            if chars[j] == '\'' {
                j += 1;
                while j < chars.len() {
                    if chars[j] == '\\' {
                        j += 2;
                    } else if chars[j] == '\'' {
                        j += 1;
                        break;
                    } else {
                        j += 1;
                    }
                }
                continue;
            }

            if chars[j] == '{' {
                depth += 1;
                found_open = true;
            } else if chars[j] == '}' {
                depth -= 1;
                if found_open && depth == 0 {
                    return i;
                }
            }

            j += 1;
        }
    }

    // If we didn't find a match, return last line
    lines.len().saturating_sub(1)
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
    let from_path = root.join(from);
    let to_path = root.join(to);

    if !from_path.is_file() {
        return Err(crate::Error::validation_invalid_argument(
            "from",
            format!("Source file does not exist: {}", from),
            None,
            None,
        ));
    }

    let content = std::fs::read_to_string(&from_path).map_err(|e| {
        crate::Error::internal_io(e.to_string(), Some(format!("read {}", from)))
    })?;

    // Locate all items in the file
    let all_items = locate_items(&content);

    // Find the requested items
    let mut found_items: Vec<&LocatedItem> = Vec::new();
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
            format!(
                "Item(s) not found in {}: {}",
                from,
                missing.join(", ")
            ),
            None,
            Some(vec![
                format!("Available items: {}", available.join(", ")),
            ]),
        ));
    }

    // Sort found items by start line (descending) so removal doesn't shift line numbers
    let mut found_items_sorted = found_items.clone();
    found_items_sorted.sort_by(|a, b| b.start_line.cmp(&a.start_line));

    // Build the extraction block
    let lines: Vec<&str> = content.lines().collect();

    // Collect uses/imports from the source file that moved items might need
    let source_uses: Vec<String> = lines.iter()
        .filter(|l| l.trim().starts_with("use "))
        .map(|l| l.to_string())
        .collect();

    // Build destination content
    let dest_exists = to_path.is_file();
    let existing_dest = if dest_exists {
        std::fs::read_to_string(&to_path).unwrap_or_default()
    } else {
        String::new()
    };

    // Determine what the moved items actually reference from source imports
    let needed_uses = find_needed_imports(&found_items_sorted, &source_uses);

    let mut dest_additions = String::new();
    if !dest_exists {
        // New file — add module doc comment and imports
        let module_name = to_path.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("module");
        dest_additions.push_str(&format!("//! {} — extracted from {}.\n\n", module_name, from));

        // Add needed imports
        for u in &needed_uses {
            dest_additions.push_str(u);
            dest_additions.push('\n');
        }
        if !needed_uses.is_empty() {
            dest_additions.push('\n');
        }
    } else {
        dest_additions.push('\n');
    }

    // Add the items (in original order)
    let mut items_in_order = found_items.clone();
    items_in_order.sort_by_key(|i| i.start_line);

    for (idx, item) in items_in_order.iter().enumerate() {
        if idx > 0 {
            dest_additions.push('\n');
        }
        dest_additions.push_str(&item.source);
        dest_additions.push('\n');
    }

    // Build modified source (remove the items)
    let mut source_lines_keep: Vec<bool> = vec![true; lines.len()];
    for item in &found_items_sorted {
        let start = item.start_line - 1; // 0-indexed
        let end = item.end_line - 1;     // 0-indexed

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

    let modified_source: String = lines.iter()
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

    // Build the result
    let items_moved: Vec<MovedItem> = items_in_order.iter().map(|item| {
        MovedItem {
            name: item.name.clone(),
            kind: item.kind.clone(),
            source_lines: (item.start_line, item.end_line),
            line_count: item.end_line - item.start_line + 1,
        }
    }).collect();

    let file_created = !dest_exists;

    // Apply if requested
    if write {
        // Create parent directory if needed
        if let Some(parent) = to_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                crate::Error::internal_io(e.to_string(), Some(format!("create dir for {}", to)))
            })?;
        }

        // Write destination
        std::fs::write(&to_path, &final_dest).map_err(|e| {
            crate::Error::internal_io(e.to_string(), Some(format!("write {}", to)))
        })?;

        // Write modified source
        std::fs::write(&from_path, &modified_source).map_err(|e| {
            crate::Error::internal_io(e.to_string(), Some(format!("write {}", from)))
        })?;

        crate::log_status!("refactor", "Moved {} item(s) from {} to {}", items_moved.len(), from, to);
    }

    Ok(MoveResult {
        items_moved,
        from_file: from.to_string(),
        to_file: to.to_string(),
        file_created,
        imports_updated: 0, // TODO: cross-file import updates
        applied: write,
    })
}

/// Determine which `use` statements from the source file are needed by the moved items.
fn find_needed_imports(items: &[&LocatedItem], source_uses: &[String]) -> Vec<String> {
    let mut needed = Vec::new();

    // Collect all identifiers referenced in the moved items' source
    let combined_source: String = items.iter().map(|i| i.source.as_str()).collect::<Vec<_>>().join("\n");

    for use_line in source_uses {
        let trimmed = use_line.trim();
        // Extract the terminal name(s) from the use statement
        let names = extract_use_names(trimmed);
        // Check if any of those names appear in the combined moved source
        let is_needed = names.iter().any(|name| {
            // Simple word-boundary check
            combined_source.contains(name)
                && !combined_source.starts_with(&format!("use {}", name))
        });
        if is_needed {
            needed.push(use_line.clone());
        }
    }

    needed
}

/// Extract the terminal name(s) from a Rust `use` statement.
///
/// `use std::path::Path;` → ["Path"]
/// `use std::collections::{HashMap, HashSet};` → ["HashMap", "HashSet"]
/// `use super::conventions::Language;` → ["Language"]
fn extract_use_names(use_stmt: &str) -> Vec<String> {
    let mut names = Vec::new();

    let body = use_stmt.strip_prefix("use ").unwrap_or(use_stmt);
    let body = body.strip_suffix(';').unwrap_or(body).trim();

    // Check for grouped imports: `foo::{A, B, C}`
    if let Some(brace_start) = body.find('{') {
        if let Some(brace_end) = body.find('}') {
            let inner = &body[brace_start + 1..brace_end];
            for segment in inner.split(',') {
                let name = segment.trim();
                // Handle `self`, renames `Foo as Bar`
                if name == "self" {
                    continue;
                }
                if let Some(alias) = name.split(" as ").nth(1) {
                    names.push(alias.trim().to_string());
                } else {
                    names.push(name.to_string());
                }
            }
        }
    } else {
        // Simple import: `use foo::Bar;`
        if let Some(last) = body.rsplit("::").next() {
            let name = last.trim();
            if let Some(alias) = name.split(" as ").nth(1) {
                names.push(alias.trim().to_string());
            } else if name != "*" {
                names.push(name.to_string());
            }
        }
    }

    names
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

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_RUST: &str = r#"//! Module doc

use std::collections::HashMap;
use std::path::Path;

use regex::Regex;

/// A structural fingerprint.
#[derive(Debug, Clone)]
pub struct FileFingerprint {
    pub relative_path: String,
    pub language: Language,
}

/// Check if import is present.
fn has_import(expected: &str, actual: &[String]) -> bool {
    actual.iter().any(|imp| imp == expected)
}

/// Language enum.
#[derive(Debug, Clone, PartialEq)]
pub enum Language {
    Rust,
    Php,
    Unknown,
}

impl Language {
    pub fn from_extension(ext: &str) -> Self {
        match ext {
            "rs" => Language::Rust,
            "php" => Language::Php,
            _ => Language::Unknown,
        }
    }
}

const INDEX_FILES: &[&str] = &["mod.rs", "lib.rs", "main.rs"];

/// Walk source files.
pub fn walk_source_files(root: &Path) -> Vec<String> {
    vec![]
}

#[cfg(test)]
mod tests {
    use super::*;
}
"#;

    #[test]
    fn locate_all_items_in_sample() {
        let items = locate_items(SAMPLE_RUST);
        let names: Vec<&str> = items.iter().map(|i| i.name.as_str()).collect();

        assert!(names.contains(&"FileFingerprint"), "Should find FileFingerprint struct, got: {:?}", names);
        assert!(names.contains(&"has_import"), "Should find has_import fn, got: {:?}", names);
        assert!(names.contains(&"Language"), "Should find Language enum, got: {:?}", names);
        assert!(names.contains(&"INDEX_FILES"), "Should find INDEX_FILES const, got: {:?}", names);
        assert!(names.contains(&"walk_source_files"), "Should find walk_source_files fn, got: {:?}", names);
    }

    #[test]
    fn item_includes_doc_comments_and_attributes() {
        let items = locate_items(SAMPLE_RUST);
        let fp = items.iter().find(|i| i.name == "FileFingerprint").unwrap();

        assert!(fp.source.contains("/// A structural fingerprint."), "Should include doc comment");
        assert!(fp.source.contains("#[derive(Debug, Clone)]"), "Should include attribute");
        assert!(fp.source.contains("pub struct FileFingerprint"), "Should include declaration");
    }

    #[test]
    fn function_boundaries_are_correct() {
        let items = locate_items(SAMPLE_RUST);
        let has_import = items.iter().find(|i| i.name == "has_import").unwrap();

        assert!(has_import.source.contains("/// Check if import is present."));
        assert!(has_import.source.contains("fn has_import("));
        assert!(has_import.source.contains("actual.iter().any"));
        // Should end with the closing brace
        assert!(has_import.source.trim().ends_with('}'));
    }

    #[test]
    fn impl_block_detected() {
        let items = locate_items(SAMPLE_RUST);
        let lang_impl = items.iter().find(|i| i.name == "Language" && matches!(i.kind, ItemKind::Impl)).unwrap();

        assert!(lang_impl.source.contains("impl Language"));
        assert!(lang_impl.source.contains("from_extension"));
    }

    #[test]
    fn const_item_detected() {
        let items = locate_items(SAMPLE_RUST);
        let idx = items.iter().find(|i| i.name == "INDEX_FILES").unwrap();

        assert!(matches!(idx.kind, ItemKind::Const));
        assert!(idx.source.contains("mod.rs"));
    }

    #[test]
    fn extract_use_names_simple() {
        assert_eq!(extract_use_names("use std::path::Path;"), vec!["Path"]);
        assert_eq!(extract_use_names("use regex::Regex;"), vec!["Regex"]);
    }

    #[test]
    fn extract_use_names_grouped() {
        let names = extract_use_names("use std::collections::{HashMap, HashSet};");
        assert_eq!(names, vec!["HashMap", "HashSet"]);
    }

    #[test]
    fn extract_use_names_with_alias() {
        let names = extract_use_names("use std::io::Result as IoResult;");
        assert_eq!(names, vec!["IoResult"]);
    }

    #[test]
    fn move_items_to_new_file() {
        let dir = std::env::temp_dir().join("homeboy_refactor_move_test");
        let _ = std::fs::create_dir_all(&dir);

        std::fs::write(dir.join("source.rs"), r#"//! Source module.

use std::path::Path;
use regex::Regex;

/// A helper function.
fn helper_one() -> bool {
    true
}

/// Another helper.
pub fn helper_two(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

/// Main logic.
pub fn main_logic() {
    // stays here
}
"#).unwrap();

        let result = move_items(
            &["helper_one", "helper_two"],
            "source.rs",
            "helpers.rs",
            &dir,
            true,
        ).unwrap();

        assert_eq!(result.items_moved.len(), 2);
        assert!(result.file_created);
        assert!(result.applied);

        // Check source was modified
        let source = std::fs::read_to_string(dir.join("source.rs")).unwrap();
        assert!(!source.contains("helper_one"), "helper_one should be removed from source");
        assert!(!source.contains("helper_two"), "helper_two should be removed from source");
        assert!(source.contains("main_logic"), "main_logic should remain in source");

        // Check destination was created
        let dest = std::fs::read_to_string(dir.join("helpers.rs")).unwrap();
        assert!(dest.contains("helper_one"), "helper_one should be in destination");
        assert!(dest.contains("helper_two"), "helper_two should be in destination");
        assert!(dest.contains("use std::path::Path;"), "Should carry over needed import");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn move_items_dry_run() {
        let dir = std::env::temp_dir().join("homeboy_refactor_move_dry_test");
        let _ = std::fs::create_dir_all(&dir);

        std::fs::write(dir.join("source.rs"), r#"fn foo() { 1 }
fn bar() { 2 }
"#).unwrap();

        let result = move_items(
            &["foo"],
            "source.rs",
            "dest.rs",
            &dir,
            false, // dry run
        ).unwrap();

        assert_eq!(result.items_moved.len(), 1);
        assert!(!result.applied);

        // Source should be unchanged
        let source = std::fs::read_to_string(dir.join("source.rs")).unwrap();
        assert!(source.contains("fn foo()"));

        // Dest should not exist
        assert!(!dir.join("dest.rs").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn move_items_missing_item_returns_error() {
        let dir = std::env::temp_dir().join("homeboy_refactor_move_missing_test");
        let _ = std::fs::create_dir_all(&dir);

        std::fs::write(dir.join("source.rs"), "fn foo() {}\n").unwrap();

        let result = move_items(
            &["nonexistent"],
            "source.rs",
            "dest.rs",
            &dir,
            false,
        );

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("not found"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn move_struct_with_derive() {
        let dir = std::env::temp_dir().join("homeboy_refactor_move_struct_test");
        let _ = std::fs::create_dir_all(&dir);

        std::fs::write(dir.join("source.rs"), r#"use serde::Serialize;

/// My struct.
#[derive(Debug, Clone, Serialize)]
pub struct MyStruct {
    pub name: String,
    pub value: usize,
}

fn other() {}
"#).unwrap();

        let result = move_items(
            &["MyStruct"],
            "source.rs",
            "types.rs",
            &dir,
            true,
        ).unwrap();

        assert_eq!(result.items_moved.len(), 1);
        assert_eq!(result.items_moved[0].name, "MyStruct");

        let dest = std::fs::read_to_string(dir.join("types.rs")).unwrap();
        assert!(dest.contains("/// My struct."));
        assert!(dest.contains("#[derive(Debug, Clone, Serialize)]"));
        assert!(dest.contains("pub struct MyStruct"));
        assert!(dest.contains("pub name: String"));
        assert!(dest.contains("use serde::Serialize;"), "Should carry over Serialize import");

        let source = std::fs::read_to_string(dir.join("source.rs")).unwrap();
        assert!(!source.contains("MyStruct"));
        assert!(source.contains("fn other()"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn move_to_existing_file_appends() {
        let dir = std::env::temp_dir().join("homeboy_refactor_move_append_test");
        let _ = std::fs::create_dir_all(&dir);

        std::fs::write(dir.join("source.rs"), "fn moved_fn() { 1 }\nfn stays() { 2 }\n").unwrap();
        std::fs::write(dir.join("dest.rs"), "//! Existing module.\n\nfn existing() { 0 }\n").unwrap();

        let result = move_items(
            &["moved_fn"],
            "source.rs",
            "dest.rs",
            &dir,
            true,
        ).unwrap();

        assert!(!result.file_created); // Appended, not created
        let dest = std::fs::read_to_string(dir.join("dest.rs")).unwrap();
        assert!(dest.contains("fn existing()"), "Should preserve existing content");
        assert!(dest.contains("fn moved_fn()"), "Should append moved function");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
