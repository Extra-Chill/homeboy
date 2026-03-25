//! types — extracted from mod.rs.

use crate::engine::codebase_scan::{
    self, find_boundary_matches, find_case_insensitive_matches, find_literal_matches,
    ExtensionFilter, ScanConfig,
};
use crate::error::{Error, Result};
use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};


/// Check if match is inside a string literal or follows a property accessor.
pub(crate) fn is_key_context(line: &str, col: usize, match_len: usize) -> bool {
    let bytes = line.as_bytes();

    // Check if preceded by `.`, `->`, or `::` (property/method access)
    let before = &line[..col];
    let trimmed = before.trim_end();
    if trimmed.ends_with('.') || trimmed.ends_with("->") || trimmed.ends_with("::") {
        return true;
    }

    // Check if inside string quotes: count unescaped quotes before the match position.
    // If an odd number of single or double quotes precede us, we're inside a string.
    let match_end = col + match_len;
    for quote in [b'\'', b'"'] {
        let mut count = 0;
        let mut i = 0;
        while i < col {
            if bytes[i] == b'\\' {
                i += 2; // skip escaped char
                continue;
            }
            if bytes[i] == quote {
                count += 1;
            }
            i += 1;
        }
        if count % 2 == 1 {
            // Verify the closing quote is after the match
            let mut j = match_end;
            while j < bytes.len() {
                if bytes[j] == b'\\' {
                    j += 2;
                    continue;
                }
                if bytes[j] == quote {
                    return true; // Inside a quoted string
                }
                j += 1;
            }
        }
    }

    false
}

/// Check if match is a variable reference (`$term` in PHP, or a standalone identifier
/// not inside strings or property access).
pub(crate) fn is_variable_context(line: &str, col: usize) -> bool {
    // PHP variable: preceded by `$`
    if col > 0 && line.as_bytes()[col - 1] == b'$' {
        return true;
    }

    // Standalone identifier: NOT inside quotes and NOT after `.`, `->`, `::`
    let before = &line[..col];
    let trimmed = before.trim_end();
    if trimmed.ends_with('.') || trimmed.ends_with("->") || trimmed.ends_with("::") {
        return false; // Property access — not a variable
    }

    // Not inside a string (simple odd-quote check)
    for quote in ['\'', '"'] {
        let count = before.chars().filter(|&c| c == quote).count();
        if count % 2 == 1 {
            return false; // Inside a string
        }
    }

    true
}

/// Check if match is inside a function parameter list.
pub(crate) fn is_parameter_context(line: &str, col: usize) -> bool {
    let before = &line[..col];

    // Look for an unclosed `(` that follows a function keyword
    let mut paren_depth: i32 = 0;
    for ch in before.chars().rev() {
        match ch {
            ')' => paren_depth += 1,
            '(' => {
                paren_depth -= 1;
                if paren_depth < 0 {
                    // We're inside an opening paren — check if it follows a function keyword
                    let before_paren = before[..before.rfind('(').unwrap_or(0)].trim_end();
                    return before_paren.ends_with("function")
                        || before_paren.ends_with("fn")
                        || before_paren.ends_with(')')  // return type: fn foo() -> Type
                        || before_paren.contains("function ")
                        || before_paren.contains("fn ");
                }
            }
            _ => {}
        }
    }

    false
}
