//! helpers — extracted from grammar_items.rs.

use super::super::grammar::{self, Grammar, Symbol};
use serde::{Deserialize, Serialize};
use super::GrammarItem;


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

/// Grammar-aware brace matching that handles strings, comments, raw strings,
/// and character/lifetime literals correctly.
///
/// This is the core replacement for the extension `find_matching_brace`.
#[allow(clippy::needless_range_loop)]
pub fn find_matching_brace(lines: &[&str], start_line: usize, grammar: &Grammar) -> usize {
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

/// Remove overlapping items (keep the one that started first / is larger).
pub(crate) fn dedupe_overlapping_items(items: Vec<GrammarItem>) -> Vec<GrammarItem> {
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
