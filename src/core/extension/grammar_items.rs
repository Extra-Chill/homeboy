//! Grammar-driven item parsing — extract top-level items with full boundaries.
//!
//! This module builds on the grammar engine (`utils/grammar.rs`) to produce
//! `GrammarItem`s — complete items with start/end lines and source text.
//! It replaces the extension-side `parse_items` command for languages that
//! have a grammar.toml.
//!
//! # Architecture
//!
//! ```text
//! utils/grammar.rs       (patterns, symbols, walk_lines)
//!     ↓
//! utils/grammar_items.rs (this file: item boundaries, source extraction)
//!     ↓
//! core/refactor/         (decompose, move — consume GrammarItems)
//! ```

use serde::{Deserialize, Serialize};

use super::grammar::self;
use super::types::Grammar;
use super::symbol::Symbol;

// ============================================================================
// Types
// ============================================================================

/// A parsed top-level item with full boundaries and source text.
///
/// This is the core equivalent of `extension::ParsedItem`, produced entirely
/// from grammar patterns without calling extension scripts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GrammarItem {
    /// Name of the item (function, struct, etc.).
    pub name: String,
    /// What kind of item: function, struct, enum, trait, impl, const, static, type_alias.
    pub kind: String,
    /// Start line (1-indexed, includes doc comments and attributes).
    pub start_line: usize,
    /// End line (1-indexed, inclusive).
    pub end_line: usize,
    /// The extracted source code (including doc comments and attributes).
    pub source: String,
    /// Visibility: "pub", "pub(crate)", "pub(super)", or "" for private.
    #[serde(default)]
    pub visibility: String,
}

// ============================================================================
// Core parse_items
// ============================================================================

/// Parse all top-level items from source content using a grammar.
///
/// This is the core replacement for the extension `parse_items` command.
/// It uses grammar patterns to find declarations, then resolves item
/// boundaries using grammar-aware brace matching that correctly handles
/// strings, comments, and language-specific constructs.
///
/// Items inside `#[cfg(test)] mod tests { ... }` blocks are excluded.
pub fn parse_items(content: &str, grammar: &Grammar) -> Vec<GrammarItem> {
    let lines: Vec<&str> = content.lines().collect();
    if lines.is_empty() {
        return Vec::new();
    }

    // Find the test module range to exclude
    let test_range = find_test_module_range(&lines, grammar);

    // Extract all symbols using the grammar engine
    let symbols = grammar::extract(content, grammar);

    // Map grammar concepts to item kinds
    let item_symbols: Vec<(&Symbol, &str)> = symbols
        .iter()
        .filter_map(|s| {
            let kind = match s.concept.as_str() {
                "function" | "free_function" => "function",
                "struct" => {
                    // The struct pattern matches struct/enum/trait — use the "kind" capture
                    s.get("kind").unwrap_or("struct")
                }
                "impl_block" => "impl",
                "type_alias" => "type_alias",
                "const_static" => s.get("kind").unwrap_or("const"),
                _ => return None,
            };
            Some((s, kind))
        })
        .collect();

    let mut items = Vec::new();

    for (symbol, kind) in &item_symbols {
        let decl_line_idx = symbol.line - 1; // 0-indexed

        // Skip if inside test module
        if let Some((test_start, test_end)) = test_range {
            if decl_line_idx >= test_start && decl_line_idx <= test_end {
                continue;
            }
        }

        // Only process top-level items (depth 0)
        if symbol.depth != 0 {
            continue;
        }

        // Find the start including doc comments and attributes
        let prefix_start = find_prefix_start(&lines, decl_line_idx);

        // Skip if prefix extends into test module
        if let Some((test_start, test_end)) = test_range {
            if prefix_start >= test_start && prefix_start <= test_end {
                continue;
            }
        }

        // Find the end of the item
        let end_line_idx = find_item_end(&lines, decl_line_idx, kind, grammar);

        // Extract the name
        let name = if *kind == "impl" {
            // For impl blocks, try type_name first
            symbol
                .get("type_name")
                .or_else(|| symbol.name())
                .unwrap_or("")
        } else {
            symbol.name().unwrap_or("")
        };

        if name.is_empty() {
            continue;
        }

        // Build the impl name with trait if present
        let full_name = if *kind == "impl" {
            if let Some(trait_name) = symbol.get("trait_name") {
                if !trait_name.is_empty() {
                    format!("{} for {}", trait_name, name)
                } else {
                    name.to_string()
                }
            } else {
                name.to_string()
            }
        } else {
            name.to_string()
        };

        // Extract visibility
        let visibility = symbol
            .visibility()
            .map(|v| v.trim().to_string())
            .unwrap_or_default();

        // Extract source text
        let source = lines[prefix_start..=end_line_idx].join("\n");

        items.push(GrammarItem {
            name: full_name,
            kind: kind.to_string(),
            start_line: prefix_start + 1, // 1-indexed
            end_line: end_line_idx + 1,   // 1-indexed
            source,
            visibility,
        });
    }

    // Sort by start line and deduplicate overlapping items
    items.sort_by_key(|item| item.start_line);
    dedupe_overlapping_items(items)
}

// ============================================================================
// Boundary detection
// ============================================================================

/// Find the start of doc comments and attributes above a declaration.
fn find_prefix_start(lines: &[&str], decl_line: usize) -> usize {
    let mut start = decl_line;

    while start > 0 {
        let prev = lines[start - 1].trim();
        if prev.starts_with("///")
            || prev.starts_with("//!")
            || prev.starts_with("#[")
            || prev.is_empty()
        {
            // Check if empty line is between doc comments (not a gap)
            if prev.is_empty() {
                // Look further back — if there's a doc comment above the blank,
                // include the blank. Otherwise stop.
                if start >= 2 {
                    let above = lines[start - 2].trim();
                    if above.starts_with("///") || above.starts_with("#[") {
                        start -= 1;
                        continue;
                    }
                }
                break;
            }
            start -= 1;
        } else {
            break;
        }
    }

    start
}

/// Find the end line of an item using grammar-aware brace matching.
#[allow(clippy::needless_range_loop)]
fn find_item_end(lines: &[&str], decl_line: usize, kind: &str, grammar: &Grammar) -> usize {
    // For const, static, type_alias — find the terminating semicolon.
    // Must handle multi-line initializers: `const X: [&str; 8] = [ ... ];`
    // The semicolon inside a type annotation like `[&str; 8]` is NOT the
    // terminating one — we need depth-aware scanning.
    if kind == "const" || kind == "static" || kind == "type_alias" {
        let mut depth: i32 = 0; // tracks [] and {} nesting
        for i in decl_line..lines.len() {
            for ch in lines[i].chars() {
                match ch {
                    '[' | '{' | '(' => depth += 1,
                    ']' | '}' | ')' => depth -= 1,
                    ';' if depth <= 0 => return i,
                    _ => {}
                }
            }
        }
        return decl_line;
    }

    // For struct/enum/trait — check if it's a unit/tuple struct (semicolon before any brace)
    if kind == "struct" || kind == "enum" || kind == "trait" {
        // Scan forward from the declaration line: if we hit `;` before `{`, it's braceless
        for i in decl_line..lines.len() {
            let line = lines[i];
            for ch in line.chars() {
                if ch == '{' {
                    // Has braces — fall through to brace matching below
                    break;
                }
                if ch == ';' {
                    return i;
                }
            }
            if line.contains('{') {
                break;
            }
        }
    }

    // For everything else — find matching brace using grammar-aware scanning
    find_matching_brace(lines, decl_line, grammar)
}

// ============================================================================
// Grammar-aware brace matching
// ============================================================================

/// Grammar-aware brace matching that handles strings, comments, raw strings,
/// and character/lifetime literals correctly.
///
/// This is the core replacement for the extension `find_matching_brace`.
#[allow(clippy::needless_range_loop)]
pub(crate) fn find_matching_brace(lines: &[&str], start_line: usize, grammar: &Grammar) -> usize {
    let open = grammar.blocks.open.chars().next().unwrap_or('{');
    let close = grammar.blocks.close.chars().next().unwrap_or('}');
    let escape_char = grammar.strings.escape.chars().next().unwrap_or('\\');
    let quote_chars: Vec<char> = grammar
        .strings
        .quotes
        .iter()
        .filter_map(|q| q.chars().next())
        .collect();

    let mut depth: i32 = 0;
    let mut found_open = false;
    let mut in_block_comment = false;
    let mut raw_string_closing: Option<String> = None;

    for i in start_line..lines.len() {
        let line = lines[i];
        let chars: Vec<char> = line.chars().collect();
        let mut j = 0;

        // If we're inside a multi-line raw string, scan for the closing delimiter
        if let Some(ref closing_str) = raw_string_closing {
            if line.contains(closing_str.as_str()) {
                raw_string_closing = None;
            }
            continue;
        }

        while j < chars.len() {
            // Inside block comment
            if in_block_comment {
                if j + 1 < chars.len() && chars[j] == '*' && chars[j + 1] == '/' {
                    in_block_comment = false;
                    j += 2;
                } else {
                    j += 1;
                }
                continue;
            }

            // Block comment start
            if j + 1 < chars.len() && chars[j] == '/' && chars[j + 1] == '*' {
                in_block_comment = true;
                j += 2;
                continue;
            }

            // Line comment
            if j + 1 < chars.len() && chars[j] == '/' && chars[j + 1] == '/' {
                break;
            }

            // Raw string literal (r#"..."#, r##"..."##, etc.)
            if chars[j] == 'r' && j + 1 < chars.len() {
                let mut hashes = 0;
                let mut k = j + 1;
                while k < chars.len() && chars[k] == '#' {
                    hashes += 1;
                    k += 1;
                }
                if k < chars.len() && chars[k] == '"' && hashes > 0 {
                    // Found r#"... — skip until matching "###
                    k += 1; // skip opening quote
                    let closing: String = std::iter::once('"')
                        .chain(std::iter::repeat_n('#', hashes))
                        .collect();
                    let closing_chars: Vec<char> = closing.chars().collect();
                    'raw_scan: while k < chars.len() {
                        if k + closing_chars.len() <= chars.len() {
                            let slice: String = chars[k..k + closing_chars.len()].iter().collect();
                            if slice == closing {
                                k += closing_chars.len();
                                break 'raw_scan;
                            }
                        }
                        k += 1;
                    }
                    // If we didn't find closing on this line, enter multi-line raw string state
                    if k >= chars.len() {
                        raw_string_closing = Some(closing);
                        break;
                    }
                    j = k;
                    continue;
                }
            }

            // Char literal: 'x', '\\', '\''
            if chars[j] == '\'' {
                let start = j;
                j += 1;
                if j < chars.len() && chars[j] == escape_char {
                    j += 2; // escaped char: '\x'
                } else if j < chars.len() {
                    j += 1; // normal char: 'x'
                }
                if j < chars.len() && chars[j] == '\'' {
                    j += 1; // closing quote
                } else {
                    // Not a valid char literal (lifetime or other) — skip the quote
                    j = start + 1;
                }
                continue;
            }

            // Regular string literal
            if quote_chars.contains(&chars[j]) {
                j += 1;
                while j < chars.len() {
                    if chars[j] == escape_char {
                        j += 2;
                    } else if chars[j] == '"' {
                        j += 1;
                        break;
                    } else {
                        j += 1;
                    }
                }
                continue;
            }

            if chars[j] == open {
                depth += 1;
                found_open = true;
            } else if chars[j] == close {
                depth -= 1;
                if found_open && depth == 0 {
                    return i;
                }
            }

            j += 1;
        }
    }

    lines.len() - 1
}

// ============================================================================
// Test module detection
// ============================================================================

/// Find the range of the `#[cfg(test)] mod tests { ... }` block.
/// Returns (start_idx, end_idx) as 0-indexed line numbers, or None.
fn find_test_module_range(lines: &[&str], grammar: &Grammar) -> Option<(usize, usize)> {
    for i in 0..lines.len() {
        if lines[i].contains("#[cfg(test)]") {
            // Look ahead for `mod tests` or `mod test`
            for j in (i + 1)..std::cmp::min(i + 3, lines.len()) {
                let trimmed = lines[j].trim();
                if trimmed.starts_with("mod tests")
                    || trimmed.starts_with("mod test ")
                    || trimmed.starts_with("mod test{")
                {
                    let end = find_matching_brace(lines, j, grammar);
                    return Some((i, end));
                }
            }
        }
    }

    // Also check for `mod tests {` without #[cfg(test)]
    for i in 0..lines.len() {
        let trimmed = lines[i].trim();
        if trimmed.starts_with("mod tests {") || trimmed.starts_with("mod tests{") {
            let end = find_matching_brace(lines, i, grammar);
            return Some((i, end));
        }
    }

    None
}

// ============================================================================
// Helpers
// ============================================================================

/// Remove overlapping items (keep the one that started first / is larger).
fn dedupe_overlapping_items(items: Vec<GrammarItem>) -> Vec<GrammarItem> {
    let mut result: Vec<GrammarItem> = Vec::new();

    for item in items {
        if let Some(last) = result.last() {
            if item.start_line >= last.start_line && item.start_line <= last.end_line {
                if (item.end_line - item.start_line) > (last.end_line - last.start_line) {
                    result.pop();
                    result.push(item);
                }
                continue;
            }
        }
        result.push(item);
    }

    result
}

/// Validate that extracted source has balanced braces.
///
/// Returns true if all braces are balanced. Use this as a pre-write
/// safety check before applying decompose/move operations.
pub fn validate_brace_balance(source: &str, grammar: &Grammar) -> bool {
    let lines: Vec<&str> = source.lines().collect();
    let open = grammar.blocks.open.chars().next().unwrap_or('{');
    let close = grammar.blocks.close.chars().next().unwrap_or('}');
    let escape_char = grammar.strings.escape.chars().next().unwrap_or('\\');
    let mut depth: i32 = 0;
    let mut in_block_comment = false;
    let mut raw_string_closing: Option<String> = None;

    for line in &lines {
        let chars: Vec<char> = line.chars().collect();
        let mut j = 0;

        // If inside a multi-line raw string, scan for closing delimiter
        if let Some(ref closing_str) = raw_string_closing {
            let line_str: String = chars.iter().collect();
            if line_str.contains(closing_str.as_str()) {
                raw_string_closing = None;
            }
            continue;
        }

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
            if j + 1 < chars.len() && chars[j] == '/' && chars[j + 1] == '*' {
                in_block_comment = true;
                j += 2;
                continue;
            }
            if j + 1 < chars.len() && chars[j] == '/' && chars[j + 1] == '/' {
                break;
            }
            // Raw string literal (r#"..."#, r##"..."##, etc.)
            if chars[j] == 'r' && j + 1 < chars.len() {
                let mut hashes = 0;
                let mut k = j + 1;
                while k < chars.len() && chars[k] == '#' {
                    hashes += 1;
                    k += 1;
                }
                if k < chars.len() && chars[k] == '"' && hashes > 0 {
                    k += 1; // skip opening quote
                    let closing: String = std::iter::once('"')
                        .chain(std::iter::repeat_n('#', hashes))
                        .collect();
                    let closing_chars: Vec<char> = closing.chars().collect();
                    let mut found_on_line = false;
                    while k < chars.len() {
                        if k + closing_chars.len() <= chars.len() {
                            let slice: String = chars[k..k + closing_chars.len()].iter().collect();
                            if slice == closing {
                                k += closing_chars.len();
                                found_on_line = true;
                                break;
                            }
                        }
                        k += 1;
                    }
                    if !found_on_line {
                        // Multi-line raw string — skip lines until closing
                        raw_string_closing = Some(closing);
                        break;
                    }
                    j = k;
                    continue;
                }
            }
            if chars[j] == '"' {
                j += 1;
                while j < chars.len() {
                    if chars[j] == escape_char {
                        j += 2;
                    } else if chars[j] == '"' {
                        j += 1;
                        break;
                    } else {
                        j += 1;
                    }
                }
                continue;
            }
            // Skip char literals: 'x', '\\', '\''
            if chars[j] == '\'' {
                j += 1;
                if j < chars.len() && chars[j] == escape_char {
                    // Escaped char: '\x' (2 chars after quote)
                    j += 2;
                } else if j < chars.len() {
                    // Normal char: 'x' (1 char after quote)
                    j += 1;
                }
                // Skip closing quote
                if j < chars.len() && chars[j] == '\'' {
                    j += 1;
                }
                continue;
            }
            if chars[j] == open {
                depth += 1;
            } else if chars[j] == close {
                depth -= 1;
            }
            j += 1;
        }
    }

    depth == 0
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    use crate::extension::grammar::{
        BlockSyntax, CommentSyntax, ConceptPattern, Grammar, LanguageMeta, StringSyntax,
    };

    /// Build a full Rust grammar with all item-relevant patterns.
    fn full_rust_grammar() -> Grammar {
        Grammar {
            language: LanguageMeta {
                id: "rust".to_string(),
                extensions: vec!["rs".to_string()],
            },
            comments: CommentSyntax {
                line: vec!["//".to_string()],
                block: vec![("/*".to_string(), "*/".to_string())],
                doc: vec!["///".to_string(), "//!".to_string()],
            },
            strings: StringSyntax {
                quotes: vec!["\"".to_string()],
                escape: "\\".to_string(),
                multiline: vec![],
            },
            blocks: BlockSyntax::default(),
            contract: None,
            patterns: {
                let mut p = HashMap::new();
                p.insert(
                    "function".to_string(),
                    ConceptPattern {
                        regex: r"^\s*(pub(?:\(crate\))?\s+)?(?:async\s+)?(?:unsafe\s+)?(?:const\s+)?fn\s+(\w+)\s*\(([^)]*)\)"
                            .to_string(),
                        captures: {
                            let mut c = HashMap::new();
                            c.insert("visibility".to_string(), 1);
                            c.insert("name".to_string(), 2);
                            c.insert("params".to_string(), 3);
                            c
                        },
                        context: "any".to_string(),
                        skip_comments: true,
                        skip_strings: true,
                        require_capture: None,
                    },
                );
                p.insert(
                    "struct".to_string(),
                    ConceptPattern {
                        regex: r"^\s*(pub(?:\(crate\))?\s+)?(struct|enum|trait)\s+(\w+)"
                            .to_string(),
                        captures: {
                            let mut c = HashMap::new();
                            c.insert("visibility".to_string(), 1);
                            c.insert("kind".to_string(), 2);
                            c.insert("name".to_string(), 3);
                            c
                        },
                        context: "top_level".to_string(),
                        skip_comments: true,
                        skip_strings: true,
                        require_capture: None,
                    },
                );
                p.insert(
                    "impl_block".to_string(),
                    ConceptPattern {
                        regex: r"^\s*impl(?:<[^>]*>)?\s+(?:(\w+)\s+for\s+)?(\w+)".to_string(),
                        captures: {
                            let mut c = HashMap::new();
                            c.insert("trait_name".to_string(), 1);
                            c.insert("type_name".to_string(), 2);
                            c
                        },
                        context: "any".to_string(),
                        skip_comments: true,
                        skip_strings: true,
                        require_capture: None,
                    },
                );
                p.insert(
                    "const_static".to_string(),
                    ConceptPattern {
                        regex: r"^\s*(pub(?:\(crate\))?\s+)?(const|static)\s+(\w+)\s*:".to_string(),
                        captures: {
                            let mut c = HashMap::new();
                            c.insert("visibility".to_string(), 1);
                            c.insert("kind".to_string(), 2);
                            c.insert("name".to_string(), 3);
                            c
                        },
                        context: "any".to_string(),
                        skip_comments: true,
                        skip_strings: true,
                        require_capture: None,
                    },
                );
                p.insert(
                    "type_alias".to_string(),
                    ConceptPattern {
                        regex: r"^\s*(pub(?:\(crate\))?\s+)?type\s+(\w+)".to_string(),
                        captures: {
                            let mut c = HashMap::new();
                            c.insert("visibility".to_string(), 1);
                            c.insert("name".to_string(), 2);
                            c
                        },
                        context: "top_level".to_string(),
                        skip_comments: true,
                        skip_strings: true,
                        require_capture: None,
                    },
                );
                p
            },
        }
    }

    #[test]
    fn parse_items_basic() {
        let content = "\
pub fn hello() {
    println!(\"hi\");
}

struct Foo {
    x: i32,
}";
        let grammar = full_rust_grammar();
        let items = parse_items(content, &grammar);

        assert_eq!(items.len(), 2);
        assert_eq!(items[0].name, "hello");
        assert_eq!(items[0].kind, "function");
        assert_eq!(items[0].start_line, 1);
        assert_eq!(items[0].end_line, 3);

        assert_eq!(items[1].name, "Foo");
        assert_eq!(items[1].kind, "struct");
        assert_eq!(items[1].start_line, 5);
        assert_eq!(items[1].end_line, 7);
    }

    #[test]
    fn parse_items_with_doc_comments() {
        let content = "\
/// This function does stuff.
/// It's important.
pub fn documented() {
    todo!()
}";
        let grammar = full_rust_grammar();
        let items = parse_items(content, &grammar);

        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "documented");
        assert_eq!(items[0].start_line, 1); // includes doc comments
        assert_eq!(items[0].end_line, 5);
        assert!(items[0].source.starts_with("/// This function"));
    }

    #[test]
    fn parse_items_with_attributes() {
        let content = "\
#[derive(Debug, Clone)]
#[serde(rename_all = \"camelCase\")]
pub struct Config {
    pub name: String,
}";
        let grammar = full_rust_grammar();
        let items = parse_items(content, &grammar);

        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "Config");
        assert_eq!(items[0].start_line, 1); // includes attributes
        assert_eq!(items[0].end_line, 5);
    }

    #[test]
    fn parse_items_skips_test_module() {
        let content = "\
pub fn real_fn() {}

#[cfg(test)]
mod tests {
    #[test]
    fn test_something() {
        assert!(true);
    }
}";
        let grammar = full_rust_grammar();
        let items = parse_items(content, &grammar);

        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "real_fn");
    }

    #[test]
    fn parse_items_impl_block() {
        let content = "\
pub struct Foo {}

impl Foo {
    pub fn new() -> Self {
        Foo {}
    }
}";
        let grammar = full_rust_grammar();
        let items = parse_items(content, &grammar);

        assert_eq!(items.len(), 2);
        assert_eq!(items[0].name, "Foo");
        assert_eq!(items[0].kind, "struct");
        assert_eq!(items[1].name, "Foo");
        assert_eq!(items[1].kind, "impl");
        assert_eq!(items[1].start_line, 3);
        assert_eq!(items[1].end_line, 7);
    }

    #[test]
    fn parse_items_trait_impl() {
        let content = "\
impl Display for Foo {
    fn fmt(&self, f: &mut Formatter) -> Result {
        write!(f, \"Foo\")
    }
}";
        let grammar = full_rust_grammar();
        let items = parse_items(content, &grammar);

        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "Display for Foo");
        assert_eq!(items[0].kind, "impl");
    }

    #[test]
    fn parse_items_const_and_type_alias() {
        let content = "\
pub const MAX_SIZE: usize = 1024;

pub type Result<T> = std::result::Result<T, Error>;

pub fn process() {}";
        let grammar = full_rust_grammar();
        let items = parse_items(content, &grammar);

        assert_eq!(items.len(), 3);
        assert_eq!(items[0].name, "MAX_SIZE");
        assert_eq!(items[0].kind, "const");
        assert_eq!(items[1].name, "Result");
        assert_eq!(items[1].kind, "type_alias");
        assert_eq!(items[2].name, "process");
        assert_eq!(items[2].kind, "function");
    }

    #[test]
    fn parse_items_const_array_multiline() {
        // Regression test for #841: const arrays with type annotations containing
        // semicolons (e.g., `[&str; 8]`) were terminated at the type annotation
        // instead of the actual closing `];`.
        let content = "\
const NOISY_DIRS: [&str; 4] = [
    \"node_modules\",
    \"dist\",
    \"vendor\",
    \"target\",
];

pub fn after() {}";
        let grammar = full_rust_grammar();
        let items = parse_items(content, &grammar);

        assert_eq!(
            items.len(),
            2,
            "Should find const + function, got: {:?}",
            items
                .iter()
                .map(|i| (&i.name, &i.kind, i.start_line, i.end_line))
                .collect::<Vec<_>>()
        );
        assert_eq!(items[0].name, "NOISY_DIRS");
        assert_eq!(items[0].kind, "const");
        assert_eq!(items[0].start_line, 1);
        assert_eq!(
            items[0].end_line, 6,
            "const array should end at `];` line (6), not at type annotation line (1)"
        );
        assert!(
            items[0].source.contains("\"target\""),
            "source should include all array elements"
        );
        assert!(
            items[0].source.ends_with("];"),
            "source should end with `];`, got: ...{}",
            &items[0].source[items[0].source.len().saturating_sub(20)..]
        );
        assert_eq!(items[1].name, "after");
        assert_eq!(items[1].kind, "function");
    }

    #[test]
    fn parse_items_const_with_braces() {
        // Const with brace-delimited initializer (e.g., HashMap literal via macro)
        let content = "\
pub static DEFAULTS: phf::Map<&str, i32> = phf::phf_map! {
    \"a\" => 1,
    \"b\" => 2,
};

pub fn after() {}";
        let grammar = full_rust_grammar();
        let items = parse_items(content, &grammar);

        assert_eq!(items.len(), 2);
        assert_eq!(items[0].name, "DEFAULTS");
        assert_eq!(items[0].kind, "static");
        assert_eq!(
            items[0].end_line, 4,
            "static with braces should end at closing line"
        );
    }

    #[test]
    fn parse_items_braces_in_string() {
        let test_content =
            "pub fn string_test() {\n    let s = \"{ not a brace }\";\n    do_stuff();\n}\n\npub fn after() {}";
        let grammar = full_rust_grammar();
        let items = parse_items(test_content, &grammar);

        assert_eq!(
            items.len(),
            2,
            "Should find 2 functions despite string braces"
        );
        assert_eq!(items[0].name, "string_test");
        assert_eq!(items[0].end_line, 4);
        assert_eq!(items[1].name, "after");
    }

    #[test]
    fn parse_items_enum_variants() {
        let content = "\
pub enum Color {
    Red,
    Green,
    Blue,
}";
        let grammar = full_rust_grammar();
        let items = parse_items(content, &grammar);

        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "Color");
        assert_eq!(items[0].kind, "enum");
        assert_eq!(items[0].start_line, 1);
        assert_eq!(items[0].end_line, 5);
    }

    #[test]
    fn parse_items_unit_struct() {
        let content = "\
pub struct Marker;

pub fn after() {}";
        let grammar = full_rust_grammar();
        let items = parse_items(content, &grammar);

        assert_eq!(items.len(), 2);
        assert_eq!(items[0].name, "Marker");
        assert_eq!(items[0].kind, "struct");
        assert_eq!(items[0].end_line, 1);
        assert_eq!(items[1].name, "after");
    }

    #[test]
    fn validate_brace_balance_works() {
        let grammar = full_rust_grammar();
        assert!(validate_brace_balance("fn foo() { bar() }", &grammar));
        assert!(validate_brace_balance(
            "fn foo() {\n    if true {\n        bar()\n    }\n}",
            &grammar
        ));
        assert!(!validate_brace_balance("fn foo() {", &grammar));
        assert!(!validate_brace_balance("fn foo() { { }", &grammar));
    }

    #[test]
    fn validate_brace_balance_char_literals() {
        let grammar = full_rust_grammar();
        // Char literal containing close brace — should NOT count as a real brace
        assert!(validate_brace_balance(
            "fn foo() { let c = '}'; }",
            &grammar
        ));
        // Char literal containing open brace
        assert!(validate_brace_balance(
            "fn foo() { let c = '{'; }",
            &grammar
        ));
        // Escaped char literal (backslash)
        assert!(validate_brace_balance(
            "fn foo() { let c = '\\\\'; }",
            &grammar
        ));
        // Escaped single quote char literal
        assert!(validate_brace_balance(
            "fn foo() { let c = '\\''; }",
            &grammar
        ));
        // rfind pattern that triggered the original bug
        assert!(validate_brace_balance(
            "fn insert_before_closing_brace(content: &str) {\n    content.rfind('}');\n}",
            &grammar
        ));
    }

    #[test]
    fn validate_brace_balance_raw_strings() {
        let grammar = full_rust_grammar();
        // Multi-line raw string containing braces — should NOT count as real braces
        assert!(validate_brace_balance(
            "fn foo() {\n    let s = r#\"\npub struct Bar {}\n\"#;\n}",
            &grammar
        ));
        // Single-line raw string with braces
        assert!(validate_brace_balance(
            "fn foo() { let s = r#\"{ not a brace }\"#; }",
            &grammar
        ));
        // Raw string with unbalanced braces inside (should still be balanced overall)
        assert!(validate_brace_balance(
            "fn foo() {\n    let s = r#\"{\n{\n{\"#;\n}",
            &grammar
        ));
    }

    #[test]
    fn find_matching_brace_skips_raw_strings() {
        let grammar = full_rust_grammar();
        // mod tests block with raw strings containing braces inside
        let content = "\
mod tests {
    fn test_something() {
        let s = r#\"
pub struct Fake {}
fn inner() { }
\"#;
        assert!(true);
    }
}

fn after() {}";
        let lines: Vec<&str> = content.lines().collect();
        let end = find_matching_brace(&lines, 0, &grammar);
        // The closing brace of `mod tests` is line 8 (0-indexed)
        // Without raw string handling, the braces inside r#"..."# would corrupt depth
        assert_eq!(
            end, 8,
            "Should find closing brace of mod tests, not be confused by raw string braces"
        );
    }
}
