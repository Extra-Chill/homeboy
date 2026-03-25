//! boundary_detection — extracted from grammar_items.rs.

use super::super::grammar::{self, Grammar, Symbol};
use serde::{Deserialize, Serialize};
use super::find_matching_brace;


/// Find the start of doc comments and attributes above a declaration.
pub(crate) fn find_prefix_start(lines: &[&str], decl_line: usize) -> usize {
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
pub(crate) fn find_item_end(lines: &[&str], decl_line: usize, kind: &str, grammar: &Grammar) -> usize {
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

/// Find the range of the `#[cfg(test)] mod tests { ... }` block.
/// Returns (start_idx, end_idx) as 0-indexed line numbers, or None.
pub(crate) fn find_test_module_range(lines: &[&str], grammar: &Grammar) -> Option<(usize, usize)> {
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
